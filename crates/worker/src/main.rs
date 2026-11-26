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
/// outbox, Wave 29, and courier status polling, Wave 30, are
/// implemented below).
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
    // Service state for the shipment poller, built from the
    // environment exactly as the API server builds its own
    // (courier credentials + DEV simulator flag), around a
    // clone of the worker's pool. `from_env` performs no
    // startup gate — the JWT-secret gate lives in `api::run`,
    // which this process never calls.
    let state = api::AppState::from_env(Some(pool.clone()));
    println!(
        "bd-marketplace worker starting: notification outbox every {seconds}s \
         (sms sender: {}, batch {})",
        notify::sender_kind(dev_mode),
        notify::DEFAULT_BATCH
    );
    println!(
        "shipment polling every {seconds}s (stale after {stale_minutes} min, \
         batch {SHIPMENT_POLL_BATCH})"
    );
    println!("planned jobs (still TODO, not yet implemented):");
    for job in PLANNED_JOBS {
        println!("  - {job}");
    }

    loop {
        match notify::process_pending(&pool, sender.as_ref(), notify::DEFAULT_BATCH).await {
            Ok(report) if report.processed() > 0 => {
                println!(
                    "notifications batch: sent {} retried {} failed {}",
                    report.sent, report.retried, report.failed
                );
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("worker: notification batch failed: {error}");
            }
        }
        match api::shipments::refresh_stale_shipments(&state, stale_minutes, SHIPMENT_POLL_BATCH)
            .await
        {
            Ok(report) if report.checked > 0 => {
                println!(
                    "shipment poll: checked {} advanced {} unchanged {} failed {}",
                    report.checked, report.advanced, report.unchanged, report.failed
                );
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("worker: shipment poll failed: {}", error.message);
            }
        }
        tokio::select! {
            _ = shutdown_signal() => {
                println!("worker: shutdown signal received — exiting");
                break;
            }
            _ = tokio::time::sleep(Duration::from_secs(seconds)) => {}
        }
    }
}
