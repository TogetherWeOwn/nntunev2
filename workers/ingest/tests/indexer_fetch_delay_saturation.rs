//! Hostile indexer-fetch chunk delays time out (saturating elapsed).
//!
//! Tests-only pin of the current `fetch_indexer` timeout clock: connect +
//! chunk `delay_ms` values accumulate with `saturating_add` against
//! `FetchConfig::timeout_ms`, so a hostile `u64::MAX` delay (or two
//! mid-size delays whose true sum overflows `u64`) saturates to `u64::MAX`
//! and aborts with [`FetchRejection::Timeout`] instead of wrapping to a
//! small elapsed value and returning partial bytes as success.
//!
//! - single chunk with `delay_ms: u64::MAX` yields
//!   `Err(FetchRejection::Timeout)`;
//! - two `2^63` chunks under `timeout_ms: u64::MAX` (true sum `2^64`,
//!   one past the budget) likewise time out — a `wrapping_add` clock
//!   would fold to `0` and succeed;
//! - small-delay control returns the body.
//!
//! Standalone suite with its own `FakeTransport`/`FakeDns`: no real
//! network, no DNS, no Cloudflare bindings. Implementation stays
//! untouched.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked -p nntune-ingest --test indexer_fetch_delay_saturation
//! ```

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};

use nntune_ingest::indexer_fetch::{
    fetch_indexer, BodyChunk, DnsResolver, FetchConfig, FetchRejection, IndexerTransport,
    PinnedRequest, RawResponse,
};

const HOST: &str = "indexer.example";
const URL: &str = "https://indexer.example/api?t=music&o=json";
const PUBLIC_V4: Ipv4Addr = Ipv4Addr::new(93, 184, 216, 34);
const BODY: &[u8] = b"hello";

#[derive(Default)]
struct FakeDns {
    map: HashMap<String, Vec<IpAddr>>,
}

impl FakeDns {
    fn public() -> Self {
        let mut dns = Self::default();
        dns.map
            .insert(HOST.to_string(), vec![IpAddr::V4(PUBLIC_V4)]);
        dns
    }
}

impl DnsResolver for FakeDns {
    fn resolve(&self, host: &str) -> Vec<IpAddr> {
        self.map.get(host).cloned().unwrap_or_default()
    }
}

#[derive(Default)]
struct FakeTransport {
    routes: HashMap<String, RawResponse>,
}

impl FakeTransport {
    fn serve(mut self, url: &str, response: RawResponse) -> Self {
        self.routes.insert(url.to_string(), response);
        self
    }
}

impl IndexerTransport for FakeTransport {
    fn get(&mut self, request: &PinnedRequest) -> Result<RawResponse, FetchRejection> {
        self.routes
            .get(&request.url)
            .cloned()
            .ok_or(FetchRejection::UpstreamStatus(404))
    }
}

fn chunk(delay_ms: u64, bytes: &[u8]) -> BodyChunk {
    BodyChunk {
        delay_ms,
        bytes: bytes.to_vec(),
    }
}

/// One hostile chunk carrying `u64::MAX` ms must time out, never succeed
/// with (partial) bytes.
#[test]
fn single_max_chunk_delay_times_out() {
    let dns = FakeDns::public();
    let response = RawResponse {
        status: 200,
        location: None,
        connect_delay_ms: 0,
        chunks: vec![chunk(u64::MAX, BODY)],
    };
    let mut transport = FakeTransport::default().serve(URL, response);
    let err = fetch_indexer(URL, &FetchConfig::default(), &dns, &mut transport)
        .expect_err("u64::MAX chunk delay must time out");
    assert_eq!(err, FetchRejection::Timeout);
    assert_eq!(err.code(), "indexer_fetch_timeout");
    assert_eq!(err.http_status(), 504);
}

/// Two mid-size (`2^63`) chunks whose true sum (`2^64`) exceeds a
/// `u64::MAX` budget time out via saturation. A wrapping clock would fold
/// the sum to `0` and return success, so this fails under a
/// `saturating_add` -> `wrapping_add` mutation.
#[test]
fn two_mid_size_chunk_delays_saturating_sum_times_out() {
    let dns = FakeDns::public();
    let half_plus_one = (u64::MAX / 2) + 1;
    assert_eq!(half_plus_one, 1u64 << 63);
    let cfg = FetchConfig {
        timeout_ms: u64::MAX,
        ..FetchConfig::default()
    };
    let response = RawResponse {
        status: 200,
        location: None,
        connect_delay_ms: 0,
        chunks: vec![chunk(half_plus_one, b"a"), chunk(half_plus_one, b"b")],
    };
    let mut transport = FakeTransport::default().serve(URL, response);
    let err = fetch_indexer(URL, &cfg, &dns, &mut transport)
        .expect_err("saturating mid-size chunk sum must time out");
    assert_eq!(err, FetchRejection::Timeout);
    assert_eq!(err.code(), "indexer_fetch_timeout");
    assert_eq!(err.http_status(), 504);
}

/// Small delays well under both budgets still fetch the body.
#[test]
fn small_delay_control_returns_body() {
    let dns = FakeDns::public();
    let response = RawResponse {
        status: 200,
        location: None,
        connect_delay_ms: 5,
        chunks: vec![chunk(5, BODY)],
    };
    let mut transport = FakeTransport::default().serve(URL, response);
    let bytes = fetch_indexer(URL, &FetchConfig::default(), &dns, &mut transport)
        .expect("in-budget fetch must succeed");
    assert_eq!(bytes, BODY);
}
