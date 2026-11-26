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
//! The worker runs the same embedded migrations as the API
//! at startup (idempotent — whichever process starts first
//! applies them). Poll interval: `WORKER_POLL_SECONDS`
//! (default 10). Shutdown is graceful on Ctrl-C / SIGTERM.
//!
//! Still planned (not implemented): courier status polling,
//! payout jobs, COD reconciliation — those jobs will call
//! the same service functions the API exposes when they land.

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;

/// Jobs this worker will own in later waves (the notification
/// outbox below is implemented, Wave 29).
const PLANNED_JOBS: &[&str] = &[
    "courier_status_polling (poll couriers, apply unified shipment statuses via the shipments refresh service)",
    "payout_jobs (compute_commission-based seller payouts on schedule)",
    "cod_reconciliation (match courier COD collections to orders)",
];

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
    println!(
        "bd-marketplace worker starting: notification outbox every {seconds}s \
         (sms sender: {}, batch {})",
        notify::sender_kind(dev_mode),
        notify::DEFAULT_BATCH
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
        tokio::select! {
            _ = shutdown_signal() => {
                println!("worker: shutdown signal received — exiting");
                break;
            }
            _ = tokio::time::sleep(Duration::from_secs(seconds)) => {}
        }
    }
}
