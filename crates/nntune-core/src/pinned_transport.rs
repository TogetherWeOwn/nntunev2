//! Pinned, cancellable NNTP transport proof for NZB fetch.
//!
//! Fail-closed policy consumed by the NZB intake wiring
//! ([TOG-26211](/TOG/issues/TOG-26211), untouched): TLS-on only, NNTP port
//! allowlist, cooperative cancellation, names-only diagnostics.
//!
//! - **Allowlist:** [`ALLOWED_NNTP_PORTS`] (`119`, `563` — the NNTP
//!   conventions; the policy additionally requires TLS-on, so `119` is only
//!   valid as a TLS port here, never as plaintext).
//! - **TLS-first order:** [`validate_nntp_endpoint`] checks `tls` BEFORE the
//!   port allowlist. A TLS-off config is [`TransportRejection::TlsRequired`]
//!   whatever the port — port policy never launders a plaintext config into
//!   a port error.
//! - **Cancel bound:** [`CANCEL_BOUND_MS`] (`1_000` ms). [`with_cancel`]
//!   resolves the fetch unless the cancel future resolves first; a cancelled
//!   session has unknown wire state, so the caller must discard the client
//!   and reconnect, never reuse it. The fetch future is dropped at cancel
//!   (no partial-output contract).
//! - **Stable codes:** [`TransportRejection::code`] returns
//!   `transport_tls_required`, `transport_port_not_allowed`
//!   (port payload never changes the code), `transport_cancelled`. The
//!   native [`crate::transports::tokio_tls::connect_pinned`] entry point runs
//!   the same validation before any TCP dial and embeds the same codes in
//!   its `io::Error` message. The general
//!   [`crate::transports::tokio_tls::connect`] stays permissive on purpose:
//!   the stream/ingest fixture harnesses and the client unit tests dial
//!   plaintext loopback servers through it (pool/stream changes are out of
//!   scope); the NZB fetch path must use `connect_pinned`.
//! - **Diagnostics are names-only:** `Display`/`Debug` render host, port and
//!   code material only — never the configured username or password.
//!
//! The shared path is `std`-only (no `tokio`, no sockets, no new crates) so
//! `cargo check -p nntune-core --target wasm32-unknown-unknown` keeps
//! passing; cancellation is implemented with [`std::future::poll_fn`].

use std::fmt;
use std::future::Future;

use crate::nntp::NntpConfig;

/// NNTP ports the fetch path accepts: `119` and `563` (TLS required
/// separately — `119` is never plaintext here).
pub const ALLOWED_NNTP_PORTS: [u16; 2] = [119, 563];

/// Bound (ms) within which [`with_cancel`] must surface a fired cancel.
/// Mirrored by the acceptance verdicts; the combinator itself resolves as
/// soon as either side resolves.
pub const CANCEL_BOUND_MS: u64 = 1_000;

/// Fail-closed transport rejection with a stable machine code.
///
/// Carries addressing only (at most the offending port): never credentials,
/// so `Display` and `Debug` are safe for logs by construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportRejection {
    /// TLS-off config (`tls == false`): rejected before connect on any port.
    TlsRequired,
    /// TLS-on config with an off-allowlist port; carries the offending port.
    PortNotAllowed(u16),
    /// The cancel future resolved before the fetch completed.
    Cancelled,
}

impl TransportRejection {
    /// Stable machine code for logs/metrics:
    /// `transport_tls_required` | `transport_port_not_allowed` |
    /// `transport_cancelled`. The port payload never changes the code.
    pub fn code(&self) -> &'static str {
        match self {
            TransportRejection::TlsRequired => "transport_tls_required",
            TransportRejection::PortNotAllowed(_) => "transport_port_not_allowed",
            TransportRejection::Cancelled => "transport_cancelled",
        }
    }
}

impl fmt::Display for TransportRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportRejection::TlsRequired => {
                write!(f, "transport_tls_required: NNTP connection requires TLS")
            }
            TransportRejection::PortNotAllowed(port) => write!(
                f,
                "transport_port_not_allowed: port {port} not in [119, 563]"
            ),
            TransportRejection::Cancelled => {
                write!(f, "transport_cancelled: NNTP fetch cancelled")
            }
        }
    }
}

impl std::error::Error for TransportRejection {}

/// Validated NNTP endpoint: addressing only (host preserved verbatim for TLS
/// SNI + dial; port always one of [`ALLOWED_NNTP_PORTS`]). Carries no
/// secrets: `Debug` renders the host/port only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedEndpoint {
    /// Server hostname, preserved verbatim for TLS SNI + dial.
    pub host: String,
    /// Validated port: always one of [`ALLOWED_NNTP_PORTS`].
    pub port: u16,
}

/// Pure, fail-closed validation: no I/O, no DNS, no clock.
///
/// Checks TLS FIRST (a TLS-off config is [`TransportRejection::TlsRequired`]
/// whatever the port), then the port allowlist. Credentials on the config
/// are ignored, never echoed.
pub fn validate_nntp_endpoint(
    config: &NntpConfig,
) -> Result<ValidatedEndpoint, TransportRejection> {
    if !config.tls {
        return Err(TransportRejection::TlsRequired);
    }
    if !ALLOWED_NNTP_PORTS.contains(&config.port) {
        return Err(TransportRejection::PortNotAllowed(config.port));
    }
    Ok(ValidatedEndpoint {
        host: config.host.clone(),
        port: config.port,
    })
}

/// Cooperative cancellation combinator for the fetch path: resolves the
/// fetch unless `cancel` resolves first.
///
/// A cancelled NNTP session has unknown wire state (the command was sent,
/// the reply was abandoned): the caller must discard the client and
/// reconnect, never reuse it. No partial-output contract: the fetch future
/// is dropped at cancel. When both sides are ready, the fetch wins.
///
/// `std`-only (no `tokio::select!`) so the shared path stays wasm-safe.
pub async fn with_cancel<F, C>(fetch: F, cancel: C) -> Result<F::Output, TransportRejection>
where
    F: Future,
    C: Future<Output = ()>,
{
    let mut fetch = std::pin::pin!(fetch);
    let mut cancel = std::pin::pin!(cancel);
    std::future::poll_fn(|cx| {
        if let std::task::Poll::Ready(out) = fetch.as_mut().poll(cx) {
            std::task::Poll::Ready(Ok(out))
        } else if cancel.as_mut().poll(cx).is_ready() {
            std::task::Poll::Ready(Err(TransportRejection::Cancelled))
        } else {
            std::task::Poll::Pending
        }
    })
    .await
}
