//! Background worker binary (Wave 1 skeleton).
//!
//! Wave 1 behaviour: log startup, list the jobs this worker will own,
//! and exit cleanly. No queue is connected yet — Redis wiring is Wave 2.
//! Each future job will call the same domain/courier/payments functions
//! the API calls; no job logic will be duplicated here.

/// Jobs this worker will own in later waves.
const PLANNED_JOBS: &[&str] = &[
    "courier_status_polling (poll Steadfast/Pathao/RedX, map to common ShipmentStatus)",
    "payout_jobs (compute_commission-based seller payouts on schedule)",
    "notifications (SMS/order notifications to buyers and sellers)",
    "cod_reconciliation (match courier COD collections to orders)",
];

#[tokio::main]
async fn main() {
    println!("bd-marketplace worker starting (Wave 1 skeleton)");
    println!("planned jobs (all TODO, not yet implemented):");
    for job in PLANNED_JOBS {
        println!("  - {job}");
    }
    // TODO(wave-2): connect to REDIS_URL, claim jobs, run them on intervals.
    println!("no queue connected in Wave 1; exiting cleanly");
}
