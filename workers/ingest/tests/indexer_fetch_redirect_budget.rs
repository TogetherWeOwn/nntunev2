//! Indexer-fetch redirect-budget + hostname-boundary pins.
//!
//! Slice [`TOG-29908`](/TOG/issues/TOG-29908): the 4th-hop refusal,
//! 253/254 hostname edge and timeout saturation were untested at
//! `df962ccd` (ports / shape / backslash arms already pinned in
//! `indexer_fetch_ssrf_guard` and the unit tests inside
//! `workers/ingest/src/indexer_fetch.rs`).
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked --manifest-path workers/ingest/Cargo.toml --test indexer_fetch_redirect_budget
//! ```
//!
//! Fixtures are synthetic and `std`-only: a fake DNS resolver and a fake
//! transport. No network, no DNS, no Cloudflare bindings.

use std::cell::RefCell;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};

use nntune_ingest::indexer_fetch::{
    fetch_indexer, validate_indexer_url, BodyChunk, DnsResolver, FetchConfig, FetchRejection,
    IndexerTransport, PinnedRequest, RawResponse, FETCH_TIMEOUT_MS, MAX_REDIRECTS,
};

const PUBLIC_V4: Ipv4Addr = Ipv4Addr::new(93, 184, 216, 34);

#[derive(Default)]
struct FakeDns {
    map: HashMap<String, Vec<IpAddr>>,
    lookups: RefCell<Vec<String>>,
}

impl FakeDns {
    fn with(host: &str, addrs: &[IpAddr]) -> Self {
        let mut dns = Self::default();
        dns.map.insert(host.to_string(), addrs.to_vec());
        dns
    }

    fn lookups(&self) -> Vec<String> {
        self.lookups.borrow().clone()
    }
}

impl DnsResolver for FakeDns {
    fn resolve(&self, host: &str) -> Vec<IpAddr> {
        self.lookups.borrow_mut().push(host.to_string());
        self.map.get(host).cloned().unwrap_or_default()
    }
}

#[derive(Default)]
struct FakeTransport {
    routes: HashMap<String, RawResponse>,
    dials: Vec<PinnedRequest>,
}

impl FakeTransport {
    fn serve(mut self, url: &str, response: RawResponse) -> Self {
        self.routes.insert(url.to_string(), response);
        self
    }
}

impl IndexerTransport for FakeTransport {
    fn get(&mut self, request: &PinnedRequest) -> Result<RawResponse, FetchRejection> {
        self.dials.push(request.clone());
        self.routes
            .get(&request.url)
            .cloned()
            .ok_or(FetchRejection::UpstreamStatus(404))
    }
}

fn ok(connect_delay_ms: u64, chunk_delay_ms: u64) -> RawResponse {
    RawResponse {
        status: 200,
        location: None,
        connect_delay_ms,
        chunks: vec![BodyChunk {
            delay_ms: chunk_delay_ms,
            bytes: b"ok".to_vec(),
        }],
    }
}

fn redirect(location: &str) -> RawResponse {
    RawResponse {
        status: 302,
        location: Some(location.to_string()),
        connect_delay_ms: 5,
        chunks: vec![],
    }
}

/// Exactly `MAX_REDIRECTS` hops succeed; one more hop is refused.
///
/// Chain shape: `start -> hop1 -> ... -> hopN -> 200`. With
/// `MAX_REDIRECTS == 3`, three redirects followed by a 200 is the last
/// legal chain; a fourth redirect on the same budget is
/// `TooManyRedirects` (stable 400 + stable code), with no fifth dial.
#[test]
fn redirect_budget_boundary_three_succeeds_four_refused() {
    assert_eq!(MAX_REDIRECTS, 3, "budget pin: MAX_REDIRECTS == 3");

    let dns = FakeDns::with("indexer.example", &[IpAddr::V4(PUBLIC_V4)]);
    let cfg = FetchConfig::default();

    // Exactly MAX_REDIRECTS redirects, then a 200: succeeds.
    let at_cap: Vec<String> = (0..=MAX_REDIRECTS)
        .map(|n| format!("https://indexer.example/cap{n}"))
        .collect();
    let mut transport = FakeTransport::default();
    for window in at_cap.windows(2) {
        transport = transport.serve(&window[0], redirect(&window[1]));
    }
    transport = transport.serve(at_cap.last().unwrap(), ok(5, 5));
    let bytes =
        fetch_indexer(&at_cap[0], &cfg, &dns, &mut transport).expect("cap chain must fetch");
    assert_eq!(bytes, b"ok".to_vec());
    assert_eq!(
        transport.dials.len(),
        (MAX_REDIRECTS + 1) as usize,
        "initial + exactly MAX_REDIRECTS hops"
    );

    // One redirect past the budget: refused, no extra dial.
    let over_cap: Vec<String> = (0..=(MAX_REDIRECTS + 1))
        .map(|n| format!("https://indexer.example/over{n}"))
        .collect();
    let mut transport = FakeTransport::default();
    for window in over_cap.windows(2) {
        transport = transport.serve(&window[0], redirect(&window[1]));
    }
    // The final URL would be a 200 if the fetcher ever reached it; the
    // refusal must land first.
    transport = transport.serve(over_cap.last().unwrap(), ok(5, 5));
    let err = fetch_indexer(&over_cap[0], &cfg, &dns, &mut transport)
        .expect_err("4-link chain must be refused at the budget");
    assert_eq!(err, FetchRejection::TooManyRedirects);
    assert_eq!(err.code(), "indexer_fetch_too_many_redirects");
    assert_eq!(err.http_status(), 400);
    assert_eq!(
        transport.dials.len(),
        (MAX_REDIRECTS + 1) as usize,
        "initial + MAX_REDIRECTS hops, then cut off before the 4th hop"
    );
}

/// Hostname length boundary: 253 accepted, 254 rejected.
///
/// Both names are realistic multi-label LDH hosts (every label <= 63);
/// only the total length differs. The 254-char host is rejected as
/// malformed before any DNS lookup; the 253-char host resolves and
/// fetches through the guard.
#[test]
fn hostname_length_boundary_253_accepted_254_rejected() {
    let host_253 = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(61)
    );
    let host_254 = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(62)
    );
    assert_eq!(host_253.len(), 253);
    assert_eq!(host_254.len(), 254);

    let url_253 = format!("https://{host_253}/api?t=music");
    let url_254 = format!("https://{host_254}/api?t=music");

    // 254-char host: malformed, no DNS, no dial.
    let dns = FakeDns::with(&host_254, &[IpAddr::V4(PUBLIC_V4)]);
    let mut transport = FakeTransport::default();
    let err = fetch_indexer(&url_254, &FetchConfig::default(), &dns, &mut transport)
        .expect_err("254-char host must be rejected");
    assert_eq!(err, FetchRejection::MalformedUrl, "254-char host");
    assert_eq!(err.http_status(), 400);
    assert!(transport.dials.is_empty(), "rejected before any dial");
    assert!(dns.lookups().is_empty(), "shape check runs before DNS");

    // 253-char host: validates, resolves exactly once, fetches.
    let dns = FakeDns::with(&host_253, &[IpAddr::V4(PUBLIC_V4)]);
    let target = validate_indexer_url(&url_253, &FetchConfig::default(), &dns)
        .expect("253-char host must validate");
    assert_eq!(target.host, host_253);
    assert_eq!(target.addresses, vec![IpAddr::V4(PUBLIC_V4)]);
    assert_eq!(dns.lookups(), vec![host_253.clone()]);

    let dns = FakeDns::with(&host_253, &[IpAddr::V4(PUBLIC_V4)]);
    let mut transport = FakeTransport::default().serve(&url_253, ok(5, 5));
    let bytes = fetch_indexer(&url_253, &FetchConfig::default(), &dns, &mut transport)
        .expect("253-char host must fetch");
    assert_eq!(bytes, b"ok".to_vec());
    assert_eq!(transport.dials.len(), 1);
}

/// Empty and whitespace redirect `Location`s are malformed.
///
/// Each case dials only the initial URL: the hostile hop is never
/// resolved and never dialed. `None` (missing Location) is a separate,
/// already-pinned arm (`UpstreamStatus`) and is not exercised here.
#[test]
fn empty_and_whitespace_location_rejected() {
    let start = "https://indexer.example/api";
    for location in [
        String::new(),
        " ".to_string(),
        "\t".to_string(),
        "https://indexer.example/a b".to_string(),
        "/next page".to_string(),
        "//indexer.example/a\tb".to_string(),
    ] {
        let dns = FakeDns::with("indexer.example", &[IpAddr::V4(PUBLIC_V4)]);
        let response = RawResponse {
            status: 302,
            location: Some(location.clone()),
            connect_delay_ms: 5,
            chunks: vec![],
        };
        let mut transport = FakeTransport::default().serve(start, response);
        let err = fetch_indexer(start, &FetchConfig::default(), &dns, &mut transport)
            .expect_err("empty/whitespace Location must be rejected");
        assert_eq!(err, FetchRejection::MalformedUrl, "location {location:?}");
        assert_eq!(err.http_status(), 400);
        assert_eq!(transport.dials.len(), 1, "only the initial dial");
    }
}

/// Saturated accumulated delays yield `Timeout`.
///
/// Every hop alone stays under both budgets; the running total reaching
/// `FETCH_TIMEOUT_MS` aborts with `Timeout` (stable 504) and never
/// returns partial bytes. At the deadline counts as expired; one
/// millisecond under still fetches. Fake DNS/transport seam only.
#[test]
fn saturated_accumulated_delays_yield_timeout() {
    // Multi-hop saturation on the pinned contract numbers: each connect
    // stays 100 ms under the 5 s dial budget, but the running total
    // crosses the 15 s fetch budget on the final chunk.
    let dns = FakeDns::with("indexer.example", &[IpAddr::V4(PUBLIC_V4)]);
    let cfg = FetchConfig::default();
    assert_eq!(cfg.timeout_ms, FETCH_TIMEOUT_MS);
    let u = |n: u8| format!("https://indexer.example/sat{n}");
    let saturated = RawResponse {
        status: 200,
        location: None,
        connect_delay_ms: 4_900,
        chunks: vec![
            BodyChunk {
                delay_ms: 4_900,
                bytes: b"partial-".to_vec(),
            },
            BodyChunk {
                delay_ms: 4_900,
                bytes: b"bytes".to_vec(),
            },
            // 4900 * 3 = 14700, + 500 = 15200 > 15000: saturates here.
            BodyChunk {
                delay_ms: 500,
                bytes: b"late".to_vec(),
            },
        ],
    };
    let mut transport = FakeTransport::default().serve(&u(0), saturated);
    let err = fetch_indexer(&u(0), &cfg, &dns, &mut transport)
        .expect_err("saturated accumulated delays must time out");
    assert_eq!(err, FetchRejection::Timeout);
    assert_eq!(err.code(), "indexer_fetch_timeout");
    assert_eq!(err.http_status(), 504);

    // Exact-deadline saturation on a small budget: reaching the total
    // counts as expired, one millisecond under still succeeds.
    let cfg = FetchConfig {
        dial_timeout_ms: 1_000,
        timeout_ms: 1_000,
        ..FetchConfig::default()
    };
    let dns = FakeDns::with("indexer.example", &[IpAddr::V4(PUBLIC_V4)]);
    let at_deadline = RawResponse {
        status: 200,
        location: None,
        connect_delay_ms: 400,
        chunks: vec![BodyChunk {
            delay_ms: 600, // 400 + 600 = 1000: at the deadline
            bytes: b"ok".to_vec(),
        }],
    };
    let mut transport = FakeTransport::default().serve("https://indexer.example/api", at_deadline);
    assert_eq!(
        fetch_indexer("https://indexer.example/api", &cfg, &dns, &mut transport),
        Err(FetchRejection::Timeout),
        "at the deadline counts as expired"
    );

    let under_budget = RawResponse {
        status: 200,
        location: None,
        connect_delay_ms: 400,
        chunks: vec![BodyChunk {
            delay_ms: 599, // 999 ms total: 1 ms under
            bytes: b"ok".to_vec(),
        }],
    };
    let mut transport = FakeTransport::default().serve("https://indexer.example/api", under_budget);
    assert_eq!(
        fetch_indexer("https://indexer.example/api", &cfg, &dns, &mut transport),
        Ok(b"ok".to_vec()),
        "1 ms under the total still fetches"
    );
}
