//! Pool dial-site pinning acceptance tests (tests-only).
//!
//! Contract under test: both Durable Object dial sites in
//! `workers/stream/src/runtime.rs` (`NntpPool::probe` and `NntpPool::body`)
//! must dial pool connections through the fail-closed pinned entry point,
//! `WorkerTransport::connect_pinned(&config)`, never the general
//! `WorkerTransport::connect(&config)`. The pinned entry enforces the
//! shared `pinned_transport` policy (TLS-first, then port allowlist) before
//! any dial; the general entry does not.
//!
//! `runtime.rs` is wasm32-only, so native `cargo test` never links it; like
//! the existing `assert_pinned_entry_wires_through_helper` guard in
//! `crates/nntune-core/src/pinned_transport.rs`, this file reads the source
//! as text via `include_str!` and pins the wiring per dial site:
//!
//! - each site's function body contains exactly one pinned dial;
//! - no site (and no other code in the file) keeps the unpinned dial;
//! - each pinned dial produces the `transport` handed to
//!   `NntpClient::connect(transport, &config)` (pinned dial first).
//!
//! Sensitivity: reverting either site to `connect`, dropping one site's
//! pinned dial, or satisfying one site twice while leaving the other
//! unpinned all fail here. Comment-only lines are stripped before pinning,
//! so a mention in a comment cannot satisfy the contract.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-stream --test runtime_dial_pinned_acceptance
//! ```

const RUNTIME_RS: &str = include_str!("../src/runtime.rs");

const PINNED_DIAL: &str = "WorkerTransport::connect_pinned(&config)";
const UNPINNED_DIAL: &str = "WorkerTransport::connect(&config)";
const CLIENT_CONNECT: &str = "NntpClient::connect(transport, &config)";

/// `runtime.rs` with comment-only lines removed, so only code can pin.
fn runtime_code() -> String {
    RUNTIME_RS
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The code from `start` up to (not including) `end`.
fn fn_slice<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let fn_start = source
        .find(start)
        .unwrap_or_else(|| panic!("runtime.rs must define {start}"));
    let rest = &source[fn_start..];
    let fn_end = rest
        .find(end)
        .unwrap_or_else(|| panic!("runtime.rs must define {end} after {start}"));
    &rest[..fn_end]
}

/// Wiring guard for one pool dial site, mirroring
/// `assert_pinned_entry_wires_through_helper`: fails if the site stops
/// dialling through the pinned entry on its way to `NntpClient::connect`.
fn assert_dial_site_wires_through_pinned(source: &str, site: &str, start: &str, end: &str) {
    let body = fn_slice(source, start, end);
    assert_eq!(
        body.matches(PINNED_DIAL).count(),
        1,
        "runtime.rs {site} dial site must dial exactly once through {PINNED_DIAL}"
    );
    assert!(
        !body.contains(UNPINNED_DIAL),
        "runtime.rs {site} dial site must not use the unpinned {UNPINNED_DIAL}"
    );
    let pinned_pos = body
        .find(PINNED_DIAL)
        .unwrap_or_else(|| panic!("runtime.rs {site} dial site must call {PINNED_DIAL}"));
    let client_pos = body.find(CLIENT_CONNECT).unwrap_or_else(|| {
        panic!("runtime.rs {site} dial site must hand its transport to {CLIENT_CONNECT}")
    });
    assert!(
        pinned_pos < client_pos,
        "runtime.rs {site} dial site must dial through connect_pinned before NntpClient::connect"
    );
}

#[test]
fn probe_dial_wires_through_connect_pinned() {
    assert_dial_site_wires_through_pinned(
        &runtime_code(),
        "probe",
        "async fn probe",
        "async fn body",
    );
}

#[test]
fn body_dial_wires_through_connect_pinned() {
    assert_dial_site_wires_through_pinned(
        &runtime_code(),
        "body",
        "async fn body",
        "async fn provider",
    );
}

#[test]
fn runtime_has_exactly_two_pinned_dial_sites_and_no_unpinned_worker_dial() {
    let code = runtime_code();
    assert_eq!(
        code.matches(PINNED_DIAL).count(),
        2,
        "runtime.rs must have exactly two {PINNED_DIAL} dial sites (probe and body)"
    );
    assert!(
        !code.contains(UNPINNED_DIAL),
        "runtime.rs must not keep any unpinned {UNPINNED_DIAL} dial"
    );
}
