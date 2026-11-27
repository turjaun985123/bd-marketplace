//! Wave 39 (security audit CR-14): the silent-skip gate.
//!
//! Every integration test in this crate is gated on
//! `DATABASE_URL` and SKIPS CLEANLY without it — the right
//! behaviour for a developer's laptop, and a dangerous one in
//! CI: a misconfigured CI job (service not wired, variable
//! renamed) would run the whole suite, execute zero integration
//! assertions, and still report green. The audit found exactly
//! that failure mode.
//!
//! This test is deliberately NOT database-gated. When the CI
//! marker is present (`CI=true`, set by GitHub Actions and most
//! CI systems) but `DATABASE_URL` is missing or empty, it
//! PANICS — a green CI run must mean the integration tests
//! really ran. Locally (no `CI` variable) it passes quietly so
//! the no-database developer flow is unchanged.
//!
//! The audit's companion item, CR-13 (all integration tests
//! share one database and truncate each other's tables), is
//! NOT addressed here — see the Wave 39 section of the README
//! for why it is deferred to its own wave.

#[test]
fn ci_environment_must_provide_database_url() {
    let in_ci = std::env::var("CI").is_ok_and(|value| {
        let value = value.trim();
        !value.is_empty() && value != "false" && value != "0"
    });
    if in_ci {
        let database_url = std::env::var("DATABASE_URL").unwrap_or_default();
        assert!(
            !database_url.trim().is_empty(),
            "CI is set but DATABASE_URL is missing/empty — every integration \
             test would silently skip and the run would be a false green. \
             Refusing to pass (Wave 39, audit CR-14)."
        );
    }
}
