//! Observability (Wave 43): the request-id + request-log
//! middleware — the OUTERMOST layer of the router (added last
//! in `build_app`), so every request the process answers
//! passes through it exactly once: health checks, CORS
//! preflights, body-limit rejections and 404s included.
//!
//! Request ids: a request arrives with an id or gets one.
//! An inbound `X-Request-Id` is honoured only when it passes
//! the pure domain rule ([`domain::validate_request_id`] —
//! 1..=64 printable-ASCII characters, no edge padding);
//! anything else (absent, oversized, control bytes, non-ASCII)
//! is replaced with a freshly minted UUID in simple form.
//! Either way the id goes back on the response as
//! `X-Request-Id`, so a client report, a proxy log and this
//! process's log can be correlated by one string. An unusable
//! client id is never an error — it is simply not honoured.
//!
//! Request logs: after the response completes, ONE line is
//! written — `request request_id=… method=… path=… status=…
//! latency_ms=…` — to stdout, or to stderr when the status is
//! 5xx (the same stdout/stderr split every other log line in
//! this codebase uses). `path` is the matched route PATTERN
//! (`/v1/orders/:id`) whenever the router matched one: it is
//! low-cardinality and can never contain a value. Unmatched
//! requests (404/405) fall back to the raw path. Deliberately
//! NEVER logged: the query string (the public tracking
//! endpoint takes a phone number in its query — that is PII),
//! request or response headers, and bodies.
//!
//! The line is written with `writeln!` to the stdio handles
//! rather than `println!`: a broken log pipe must never panic
//! a request, so write failures are dropped on the floor. This
//! codebase has no tracing subscriber (both binaries log via
//! stdio end to end), so the structured key=value fields are
//! printed directly — a log shipper can parse them, and
//! nothing is emitted into a framework that would silently
//! swallow it.

use std::io::Write;
use std::time::Instant;

use axum::{
    extract::{MatchedPath, Request},
    http::{HeaderName, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

/// The correlation header, both directions (Wave 43).
const REQUEST_ID_HEADER: &str = "x-request-id";

fn request_id_header_name() -> HeaderName {
    HeaderName::from_static(REQUEST_ID_HEADER)
}

/// The id this request will be known by: the inbound header
/// value when it exists, is valid UTF-8 header text and
/// passes the domain rule; otherwise a freshly minted UUID
/// (simple form — 32 hex characters, itself always a valid
/// request id under the rule).
fn resolve_request_id(request: &Request) -> String {
    let honoured = request
        .headers()
        .get(request_id_header_name())
        .and_then(|value| value.to_str().ok())
        .filter(|value| domain::validate_request_id(value))
        .map(str::to_owned);
    honoured.unwrap_or_else(|| Uuid::new_v4().simple().to_string())
}

/// The path the log line names: the matched route pattern
/// when the router matched a route (the pattern is inserted
/// into the request extensions during routing), else the raw
/// request path. Neither form ever includes the query string.
fn log_path(request: &Request) -> String {
    match request.extensions().get::<MatchedPath>() {
        Some(matched) => matched.as_str().to_owned(),
        None => request.uri().path().to_owned(),
    }
}

/// Write the one structured line for a finished request.
/// Server errors go to stderr, everything else to stdout —
/// mirroring the split the worker and the startup logs use.
fn emit_request_log(
    request_id: &str,
    method: &Method,
    path: &str,
    status: StatusCode,
    latency_ms: u128,
) {
    let line = format!(
        "request request_id={request_id} method={method} path={path} \
         status={} latency_ms={latency_ms}",
        status.as_u16()
    );
    if status.is_server_error() {
        let _ = writeln!(std::io::stderr(), "{line}");
    } else {
        let _ = writeln!(std::io::stdout(), "{line}");
    }
}

/// The middleware itself (Wave 43): resolve the request id,
/// run the request, echo the id on the response, then log
/// one line. Nothing on this path may fail the request —
/// the header insert is attempted only when the id encodes
/// as a header value (validated and minted ids always do),
/// and the log write ignores its own errors.
pub async fn request_logging(request: Request, next: Next) -> Response {
    let started = Instant::now();
    let request_id = resolve_request_id(&request);
    let method = request.method().clone();
    let path = log_path(&request);

    let mut response = next.run(request).await;

    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response
            .headers_mut()
            .insert(request_id_header_name(), value);
    }
    let status = response.status();
    emit_request_log(
        &request_id,
        &method,
        &path,
        status,
        started.elapsed().as_millis(),
    );
    response
}
