//! Hostile NZB-fetch chunk-delay saturation pin (tests-only).
//!
//! Refill-19 slice of TOG-30191: pins the current `fetch_nzb` reported-delay
//! accounting against hostile `delay_ms` values without touching the
//! implementation. The fetcher accumulates per-chunk `delay_ms` with
//! `saturating_add` and rejects at or beyond `config.timeout_ms`; this suite
//! pins that a saturating sum — never a wrap — drives the timeout.
//!
//! ```sh
//! cargo test --locked -p nntune-ingest --test nzb_fetch_delay_saturation
//! ```

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};

use nntune_ingest::nzb_fetch::{
    fetch_nzb, BodyChunk, DnsResolver, FetchConfig, FetchRejection, FetchTransport, RawResponse,
};

const ALLOWED_HOST: &str = "nzb.example";
const ALLOWED_URL: &str = "https://nzb.example/get/abc123.nzb";
const PUBLIC_V4: Ipv4Addr = Ipv4Addr::new(93, 184, 216, 34);

#[derive(Default)]
struct FakeDns {
    map: HashMap<String, Vec<IpAddr>>,
}

impl FakeDns {
    fn public(host: &str) -> Self {
        let mut dns = Self::default();
        dns.map
            .insert(host.to_string(), vec![IpAddr::V4(PUBLIC_V4)]);
        dns
    }
}

impl DnsResolver for FakeDns {
    fn resolve(&self, host: &str) -> Vec<IpAddr> {
        self.map.get(host).cloned().unwrap_or_default()
    }
}

struct ScriptedTransport {
    responses: Vec<RawResponse>,
}

impl FetchTransport for ScriptedTransport {
    fn get(&mut self, _url: &str) -> Result<RawResponse, FetchRejection> {
        Ok(self.responses.remove(0))
    }
}

fn config_with_timeout(timeout_ms: u64) -> FetchConfig {
    FetchConfig {
        allowed_hosts: vec![ALLOWED_HOST.to_string()],
        timeout_ms,
        ..FetchConfig::default()
    }
}

fn chunk(delay_ms: u64, bytes: &[u8]) -> BodyChunk {
    BodyChunk {
        delay_ms,
        bytes: bytes.to_vec(),
    }
}

fn ok_response(chunks: Vec<BodyChunk>) -> RawResponse {
    RawResponse {
        status: 200,
        location: None,
        chunks,
    }
}

/// One hostile chunk reporting `u64::MAX` ms saturates the accumulator and
/// must time out — never succeed, hang, or panic on overflow.
#[test]
fn single_chunk_max_delay_times_out() {
    let dns = FakeDns::public(ALLOWED_HOST);
    let cfg = config_with_timeout(10_000);
    let mut transport = ScriptedTransport {
        responses: vec![ok_response(vec![chunk(u64::MAX, b"<nzb/>")])],
    };
    let err = fetch_nzb(ALLOWED_URL, &cfg, &dns, &mut transport)
        .expect_err("u64::MAX chunk delay must time out");
    assert_eq!(err, FetchRejection::Timeout);
    assert_eq!(err.code(), "nzb_fetch_timeout");
    assert_eq!(err.http_status(), 504);
}

/// Mid-size chunks that stay under the budget individually but cross it
/// together must time out; no partial body is returned.
#[test]
fn accumulated_mid_size_chunks_past_budget_time_out() {
    let dns = FakeDns::public(ALLOWED_HOST);
    let cfg = config_with_timeout(1_000);
    let mut transport = ScriptedTransport {
        responses: vec![ok_response(vec![
            chunk(400, b"<?xml"),
            // Cumulative 1_100 ms > 1_000 ms budget: abort here.
            chunk(700, b" version?>"),
            chunk(10, b"<nzb></nzb>"),
        ])],
    };
    let err = fetch_nzb(ALLOWED_URL, &cfg, &dns, &mut transport)
        .expect_err("accumulated delays past the budget must time out");
    assert_eq!(err, FetchRejection::Timeout);
    assert_eq!(err.code(), "nzb_fetch_timeout");
    assert_eq!(err.http_status(), 504);
}

/// Accumulation at the `u64` boundary must saturate, not wrap: with a
/// `u64::MAX` budget, delays summing past `u64::MAX` time out. A
/// `wrapping_add` mutant would wrap to a small elapsed value and wrongly
/// succeed — this test fails under that mutation.
#[test]
fn saturating_accumulation_at_u64_boundary_times_out() {
    let dns = FakeDns::public(ALLOWED_HOST);
    let cfg = config_with_timeout(u64::MAX);
    let mut transport = ScriptedTransport {
        responses: vec![ok_response(vec![
            // Individually under the budget; together past `u64::MAX`.
            chunk(u64::MAX - 100, b"<?xml"),
            chunk(200, b" version?>"),
        ])],
    };
    let err = fetch_nzb(ALLOWED_URL, &cfg, &dns, &mut transport)
        .expect_err("saturating sum past u64::MAX must time out");
    assert_eq!(err, FetchRejection::Timeout);
}

/// Small-delay control: the same shape well inside the budget returns the
/// full body, proving the timeouts above are the delay accounting and not
/// the content.
#[test]
fn small_delay_control_returns_body() {
    let dns = FakeDns::public(ALLOWED_HOST);
    let cfg = config_with_timeout(1_000);
    let mut transport = ScriptedTransport {
        responses: vec![ok_response(vec![
            chunk(400, b"<?xml"),
            chunk(400, b" version?><nzb/>"),
        ])],
    };
    let body =
        fetch_nzb(ALLOWED_URL, &cfg, &dns, &mut transport).expect("in-budget delays must fetch");
    assert_eq!(body, b"<?xml version?><nzb/>".to_vec());
}
