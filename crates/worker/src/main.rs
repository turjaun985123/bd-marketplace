//! Background worker binary.
//!
//! Wave 29 gives the worker its first real job: the
//! notification outbox. State-changing API service functions
//! queue SMS rows in the `notifications` table inside their
//! own transactions; this process claims pending rows in
//! batches (`notify::process_pending` — `FOR UPDATE SKIP
//! LOCKED`, at most [`notify::MAX_ATTEMPTS`] attempts per
//! row) and sends them through the ONE sender selection the
//! API also uses for OTP codes (`notify::sender_from_env`):
//! the log sender in dev mode, the honestly-unconfigured
//! sender otherwise — so without a real SMS gateway the rows
//! accumulate attempts and land in `failed` with
//! `sms_not_configured` recorded, visible in the admin
//! notifications view, never silently dropped.
//!
//! Wave 30 gives the worker its second real job: automatic
//! courier status polling. Each cycle also runs
//! `api::shipments::refresh_stale_shipments` — the worker
//! depends on the api crate as a library and calls the same
//! refresh service function the seller/buyer refresh endpoint
//! calls, so a status learned by polling lands (shipment
//! event, sub-order sync, buyer notification) exactly like a
//! status learned by hand. Only API-courier shipments that
//! are stale (`SHIPMENT_POLL_STALE_MINUTES`, default 15) and
//! not in a terminal status are polled; manual shipments are
//! never polled (they have no API — the seller-entered status
//! is the truth). The service state is built with
//! `AppState::from_env`, which reads the courier credentials
//! and the DEV simulator flag from the same environment the
//! API server uses; the Wave 28 JWT-secret startup gate lives
//! in `api::run`, which the worker never calls, so it is not
//! triggered here (the worker serves no HTTP and issues no
//! tokens).
//!
//! Wave 36 gives the worker its third real job: the
//! subscription renewal sweep. Each cycle also runs
//! `api::subscriptions::process_subscriptions` over the same
//! service state: a live paid plan whose period has ended
//! renews from the seller's earnings balance when
//! auto-renew is on and the balance covers the current
//! price (extending 30 days from the OLD expiry, so a late
//! sweep never steals paid days), and otherwise lapses to
//! `expired`, dropping the store back to the free plan. Like
//! the other two jobs it calls the same service function the
//! API's rules live in — there is no second implementation
//! of the money rules here.
//!
//! Wave 48 closes the Wave 29 outbox edge: each cycle now
//! FIRST runs `notify::reap_stale_sending`, which recovers
//! rows a previous worker claimed (`sending`) and then
//! died on — a claim older than
//! `NOTIFICATION_SENDING_STALE_MINUTES` (default 10) goes
//! back to `pending`, or to `failed` when the lost claim
//! spent the row's last attempt. Running it before
//! `process_pending` means a recovered row can be claimed
//! and sent in the same cycle it is reaped. Its counts
//! ride the Wave 43 summary line (`reaped_pending` /
//! `reaped_failed`, inside the notifications group).
//!
//! Wave 43 gives the worker an observability line: each
//! cycle ends with ONE summary line carrying the three
//! jobs' headline counts (notifications sent / retried /
//! failed / reaped_pending / reaped_failed, shipments
//! checked / advanced / unchanged / failed, subscriptions
//! checked / renewed / expired).
//! It is printed EVERY cycle, not only when something
//! moved — the previous per-job lines appeared only on a
//! non-empty report, so an idle worker and a dead worker
//! looked exactly alike in the log. A job that errors
//! still reports on stderr as it happens and counts as
//! zeroes in that cycle's summary; the summary never
//! claims work the error line contradicts.
//!
//! Wave 57 adds a one-shot subcommand: `worker seed-demo`
//! builds the demo catalogue (eight demo stores, 160+
//! products) through the API crate's own service
//! functions — `api::demo_seed::run_demo_seed` — and
//! exits. It refuses to run unless `DEMO_SEED=1` is set:
//! the seed writes seller accounts, stores and a full
//! catalogue, and must never fire from a stray argument
//! in a production worker invocation. The subcommand is
//! thin arg glue over the service function (the
//! integration suite tests the function directly).
//!
//! The worker runs the same embedded migrations as the API
//! at startup (idempotent — whichever process starts first
//! applies them). Poll interval: `WORKER_POLL_SECONDS`
//! (default 10). Shutdown is graceful on Ctrl-C / SIGTERM.
//!
//! Still planned (not implemented): payout jobs, COD
//! reconciliation — those jobs will call the same service
//! functions the API exposes when they land.

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;

/// Jobs this worker will own in later waves (the notification
/// outbox, Wave 29, courier status polling, Wave 30, and the
/// subscription renewal sweep, Wave 36, are implemented
/// below).
const PLANNED_JOBS: &[&str] = &[
    "payout_jobs (compute_commission-based seller payouts on schedule)",
    "cod_reconciliation (match courier COD collections to orders)",
];

/// How many stale shipments one poll cycle refreshes at
/// most. The platform's courier accounts are shared across
/// every seller, so a cycle's worth of tracking calls stays
/// bounded; whatever does not fit is simply older next
/// cycle and goes first (the service takes oldest-first).
const SHIPMENT_POLL_BATCH: i64 = 50;

/// The poll interval in seconds: `WORKER_POLL_SECONDS`,
/// default 10; an unparsable or zero value keeps the
/// default rather than hot-looping the database.
fn poll_seconds() -> u64 {
    match std::env::var("WORKER_POLL_SECONDS") {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(seconds) if seconds >= 1 => seconds,
            _ => {
                eprintln!(
                    "WARNING: WORKER_POLL_SECONDS='{raw}' is invalid — using the default 10"
                );
                10
            }
        },
        Err(_) => 10,
    }
}

/// How stale a shipment's stored status may get before the
/// poller refreshes it, in minutes:
/// `SHIPMENT_POLL_STALE_MINUTES`, default 15; an unparsable
/// or negative value keeps the default. Zero is valid and
/// means "every non-terminal API shipment is stale" —
/// useful for a catch-up run, noisy as a steady state.
fn shipment_poll_stale_minutes() -> i64 {
    match std::env::var("SHIPMENT_POLL_STALE_MINUTES") {
        Ok(raw) => match raw.parse::<i64>() {
            Ok(minutes) if minutes >= 0 => minutes,
            _ => {
                eprintln!(
                    "WARNING: SHIPMENT_POLL_STALE_MINUTES='{raw}' is invalid — using the default 15"
                );
                15
            }
        },
        Err(_) => 15,
    }
}

/// How old a notification's claim may get before the
/// reaper treats the claiming worker as dead, in minutes:
/// `NOTIFICATION_SENDING_STALE_MINUTES`, default
/// `notify::DEFAULT_SENDING_STALE_MINUTES` (10); an
/// unparsable or zero value keeps the default. Zero is
/// NOT a valid setting here (unlike the shipment
/// staleness): zero would reap rows another worker
/// claimed seconds ago and is still sending.
fn notification_sending_stale_minutes() -> i64 {
    match std::env::var("NOTIFICATION_SENDING_STALE_MINUTES") {
        Ok(raw) => match raw.parse::<i64>() {
            Ok(minutes) if minutes >= 1 => minutes,
            _ => {
                eprintln!(
                    "WARNING: NOTIFICATION_SENDING_STALE_MINUTES='{raw}' is invalid \
                     — using the default {}",
                    notify::DEFAULT_SENDING_STALE_MINUTES
                );
                notify::DEFAULT_SENDING_STALE_MINUTES
            }
        },
        Err(_) => notify::DEFAULT_SENDING_STALE_MINUTES,
    }
}

/// SMS dev mode, read exactly as the API reads it
/// (`AppState::from_env`): `APP_ENV=dev` or
/// `SMS_DEV_MODE=true`. One flag drives both processes, so
/// OTP codes and notifications always use the same sender.
fn sms_dev_mode() -> bool {
    std::env::var("APP_ENV").map(|v| v == "dev").unwrap_or(false)
        || std::env::var("SMS_DEV_MODE").map(|v| v == "true").unwrap_or(false)
}

/// Resolve on Ctrl-C on any platform, plus SIGTERM on Unix —
/// what Docker and systemd send. Mirrors the API's shutdown
/// signal (the api crate's `shutdown_signal`).
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install the Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install the SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[tokio::main]
async fn main() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!(
                "worker: DATABASE_URL is not set — the worker needs the database \
                 (it drains the notification outbox); exiting"
            );
            std::process::exit(2);
        }
    };
    let pool = match PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
    {
        Ok(pool) => pool,
        Err(error) => {
            eprintln!("worker: could not connect to the database: {error}");
            std::process::exit(2);
        }
    };
    // The same embedded migrations the API runs (idempotent):
    // whichever process starts first applies them.
    if let Err(error) = sqlx::migrate!("../../migrations").run(&pool).await {
        eprintln!("worker: migrations failed: {error}");
        std::process::exit(2);
    }

    let dev_mode = sms_dev_mode();
    let sender = notify::sender_from_env(dev_mode);
    let seconds = poll_seconds();
    let stale_minutes = shipment_poll_stale_minutes();
    let sending_stale_minutes = notification_sending_stale_minutes();
    // Service state for the shipment poller and the
    // subscription sweep, built from the environment exactly
    // as the API server builds its own (courier credentials +
    // DEV simulator flag + the plan price/cap fallbacks),
    // around a clone of the worker's pool. `from_env`
    // performs no startup gate — the JWT-secret gate lives
    // in `api::run`, which this process never calls.
    let state = api::AppState::from_env(Some(pool.clone()));
    // Wave 57: the one-shot demo seed subcommand. Checked
    // after the pool, migrations and state exist (the seed
    // runs through the service functions, which need all
    // three) and before the poll loop starts; it exits
    // rather than falling through into the loop.
    if std::env::args().nth(1).as_deref() == Some("seed-demo") {
        if std::env::var("DEMO_SEED").map(|v| v == "1").unwrap_or(false) {
            match api::demo_seed::run_demo_seed(&state).await {
                Ok(report) => {
                    println!(
                        "seed-demo complete: stores created {}, reused {}; \
                         products created {}, updated {}, errored {}; \
                         placeholder images attached {}",
                        report.stores_created,
                        report.stores_reused,
                        report.products_created,
                        report.products_updated,
                        report.products_errored,
                        report.images_attached
                    );
                    for store in &report.stores {
                        match &store.error {
                            Some(error) => {
                                println!("  - {}: ERROR {error}", store.store_slug)
                            }
                            None => println!(
                                "  - {}: created {}, updated {}, errored {}, images {}",
                                store.store_slug,
                                store.products_created,
                                store.products_updated,
                                store.products_errored,
                                store.images_attached
                            ),
                        }
                    }
                    let failed = report.products_errored > 0
                        || report.stores.iter().any(|store| store.error.is_some());
                    std::process::exit(if failed { 1 } else { 0 });
                }
                Err(error) => {
                    eprintln!("seed-demo failed: {}", error.message);
                    std::process::exit(1);
                }
            }
        }
        eprintln!(
            "seed-demo: refusing to run without DEMO_SEED=1 — the seed writes \
             demo seller accounts, stores and a full catalogue"
        );
        std::process::exit(2);
    }
    println!(
        "bd-marketplace worker starting: notification outbox every {seconds}s \
         (sms sender: {}, batch {}, reap 'sending' after {sending_stale_minutes} min)",
        notify::sender_kind(dev_mode),
        notify::DEFAULT_BATCH
    );
    println!(
        "shipment polling every {seconds}s (stale after {stale_minutes} min, \
         batch {SHIPMENT_POLL_BATCH})"
    );
    println!("subscription renewal sweep every {seconds}s");
    println!("planned jobs (still TODO, not yet implemented):");
    for job in PLANNED_JOBS {
        println!("  - {job}");
    }

    loop {
        // Wave 43: the three jobs run as before (each error
        // still reports itself on stderr the moment it
        // happens), but their reports are collected and the
        // cycle closes with ONE summary line — see the
        // module doc for why it prints every cycle.
        //
        // Wave 48: the reaper runs BEFORE the outbox drain,
        // so rows recovered from a dead worker's claim can
        // be claimed and sent by this same cycle's
        // process_pending call.
        let reap_counts = match notify::reap_stale_sending(&pool, sending_stale_minutes).await {
            Ok(report) => (report.returned_to_pending, report.marked_failed),
            Err(error) => {
                eprintln!("worker: notification reaper failed: {error}");
                (0, 0)
            }
        };
        let notify_counts =
            match notify::process_pending(&pool, sender.as_ref(), notify::DEFAULT_BATCH).await {
                Ok(report) => (report.sent, report.retried, report.failed),
                Err(error) => {
                    eprintln!("worker: notification batch failed: {error}");
                    (0, 0, 0)
                }
            };
        let poll_counts = match api::shipments::refresh_stale_shipments(
            &state,
            stale_minutes,
            SHIPMENT_POLL_BATCH,
        )
        .await
        {
            Ok(report) => (
                report.checked,
                report.advanced,
                report.unchanged,
                report.failed,
            ),
            Err(error) => {
                eprintln!("worker: shipment poll failed: {}", error.message);
                (0, 0, 0, 0)
            }
        };
        let sub_counts = match api::subscriptions::process_subscriptions(&state).await {
            Ok(report) => (report.checked, report.renewed, report.expired),
            Err(error) => {
                eprintln!("worker: subscription sweep failed: {}", error.message);
                (0, 0, 0)
            }
        };
        // Wave 51 extension: the media orphan reaper —
        // objects no product_images / reviews / stores row
        // accounts for (left by uploads that died between
        // the object writes and the row insert) are
        // deleted. Idempotent and conservative (an object
        // dies only when the database positively does not
        // name it), so it runs every cycle like the rest.
        let media_counts = match api::product_images::reap_orphan_media(&state).await {
            Ok(report) => (report.scanned, report.deleted),
            Err(error) => {
                eprintln!("worker: media reaper failed: {}", error.message);
                (0, 0)
            }
        };
        // The media library's size at this cycle (Wave 51
        // extension): the operator's growth signal in the
        // same one-line summary — stored uploads and their
        // total original bytes, straight from the rows.
        let media_library = match &state.pool {
            Some(pool) => sqlx::query_as::<_, (i64, i64)>(
                "SELECT COUNT(*)::bigint, \
                 COALESCE(SUM(size_bytes), 0)::bigint \
                 FROM product_images WHERE storage_key IS NOT NULL",
            )
            .fetch_one(pool)
            .await
            .unwrap_or((0, 0)),
            None => (0, 0),
        };
        println!(
            "worker cycle: notifications sent={} retried={} failed={} \
             reaped_pending={} reaped_failed={} \
             shipments checked={} advanced={} unchanged={} failed={} \
             subscriptions checked={} renewed={} expired={} \
             media scanned={} orphans_deleted={} \
             media_library images={} bytes={}",
            notify_counts.0,
            notify_counts.1,
            notify_counts.2,
            reap_counts.0,
            reap_counts.1,
            poll_counts.0,
            poll_counts.1,
            poll_counts.2,
            poll_counts.3,
            sub_counts.0,
            sub_counts.1,
            sub_counts.2,
            media_counts.0,
            media_counts.1,
            media_library.0,
            media_library.1,
        );
        tokio::select! {
            _ = shutdown_signal() => {
                println!("worker: shutdown signal received — exiting");
                break;
            }
            _ = tokio::time::sleep(Duration::from_secs(seconds)) => {}
        }
    }
}
