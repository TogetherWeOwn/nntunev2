//! Acceptance contract for the live NZB URL HTTPS boundary.
//!
//! Unlike `nzb_fetch_ssrf_guard`, this suite must exercise the runtime
//! transport interface: the historical URL-only `FetchTransport` returns a
//! fully buffered response and cannot demonstrate pinned dialing, TLS identity,
//! incremental limits, or cancellation. Keep all fixtures local and deterministic.
//!
//! The live transport and URL route are intentionally not present in this
//! workspace slice. This test target is therefore an expected RED handoff until
//! the Builder provides `nzb_fetch::https_transport`; it must not be made green
//! by adapting the old buffered simulator. URL-route assertions are excluded
//! because there is no route contract or runtime `NZB_FETCH_ALLOWLIST` reader
//! in this slice. `FetchConfig::default()` remains the library's deny-all
//! policy; parsing a supplied allowlist string is not environment wiring.
//!
//! Contract exercised below: the fetch driver validates each redirect hop and
//! hands the already-vetted addresses, original hostname, remaining monotonic
//! budget, and cancellation token to a transport that never follows redirects
//! itself. The transport streams chunks to a bounded sink and uses the original
//! host for SNI, certificate verification, and HTTP Host. The scripted
//! connector is below the concrete transport boundary, not a URL-only fake
//! FetchTransport.

use std::{
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Mutex},
    time::Duration,
};

use nntune_ingest::nzb_fetch::https_transport::{
    fetch_nzb_streaming, Cancellation, Clock, ConnectRequest, Connector, ConnectorError,
    HttpsTransport, ResponseHead, StreamChunk, TransportConfig,
};
use nntune_ingest::nzb_fetch::{DnsResolver, FetchConfig, FetchRejection};

const HOST: &str = "nzb.example";
const CDN: &str = "cdn.example";
const IP_A: IpAddr = IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34));
const IP_B: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 18));
const START: &str = "https://nzb.example/start?token=private";

#[derive(Clone)]
struct FixtureDns {
    answers: Vec<(String, Vec<IpAddr>)>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl FixtureDns {
    fn new(answers: &[(&str, &[IpAddr])]) -> Self {
        Self {
            answers: answers
                .iter()
                .map(|(host, ips)| ((*host).to_owned(), ips.to_vec()))
                .collect(),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl DnsResolver for FixtureDns {
    fn resolve(&self, host: &str) -> Vec<IpAddr> {
        self.calls.lock().unwrap().push(host.to_owned());
        self.answers
            .iter()
            .find(|(name, _)| name == host)
            .map(|(_, ips)| ips.clone())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
enum Step {
    Reply(ResponseHead, Vec<Result<Vec<u8>, ConnectorError>>),
    Delayed(Duration, ResponseHead, Vec<Result<Vec<u8>, ConnectorError>>),
    Fail(ConnectorError),
}

#[derive(Clone, Default)]
struct FixtureClock(Arc<Mutex<Duration>>);

impl FixtureClock {
    fn advance(&self, elapsed: Duration) {
        let mut now = self.0.lock().unwrap();
        *now += elapsed;
    }
}

#[derive(Default)]
struct ScriptedConnector {
    steps: Vec<Step>,
    requests: Vec<ConnectRequest>,
    clock: FixtureClock,
}

impl Connector for ScriptedConnector {
    fn connect(
        &mut self,
        request: ConnectRequest,
    ) -> Result<(ResponseHead, Box<dyn StreamChunk>), ConnectorError> {
        self.requests.push(request);
        match self.steps.remove(0) {
            Step::Reply(head, chunks) => Ok((head, Box::new(chunks.into_iter()))),
            Step::Delayed(elapsed, head, chunks) => {
                self.clock.advance(elapsed);
                Ok((head, Box::new(chunks.into_iter())))
            }
            Step::Fail(error) => Err(error),
        }
    }
}

impl Clock for FixtureClock {
    fn now(&self) -> Duration {
        *self.0.lock().unwrap()
    }
}

fn config() -> FetchConfig {
    FetchConfig {
        allowed_hosts: vec![HOST.into(), CDN.into()],
        ..FetchConfig::default()
    }
}

fn transport(connector: ScriptedConnector) -> HttpsTransport<ScriptedConnector, FixtureClock> {
    let clock = connector.clock.clone();
    HttpsTransport::new(
        connector,
        clock,
        TransportConfig {
            timeout: Duration::from_secs(2),
            max_redirects: 3,
            max_bytes: 16,
            max_redirect_body_bytes: 128,
            max_error_body_bytes: 128,
        },
    )
}

#[test]
fn redirect_hops_use_each_vetted_set_and_preserve_original_tls_identity() {
    let dns = FixtureDns::new(&[(HOST, &[IP_A]), (CDN, &[IP_B])]);
    let connector = ScriptedConnector {
        steps: vec![
            Step::Reply(
                ResponseHead::redirect(302, "https://cdn.example/file.nzb"),
                vec![],
            ),
            Step::Reply(ResponseHead::ok(200), vec![Ok(b"<nzb/>".to_vec())]),
        ],
        ..Default::default()
    };
    let mut client = transport(connector);
    let cancel = Cancellation::new();

    let result = fetch_nzb_streaming(START, &config(), &dns, &mut client, &cancel);
    assert_eq!(result.unwrap(), b"<nzb/>");
    let requests = client.connector().requests();
    assert_eq!(
        requests.len(),
        2,
        "transport must not auto-follow redirects"
    );
    assert_eq!(requests[0].addresses(), &[IP_A]);
    assert_eq!(requests[1].addresses(), &[IP_B]);
    for request in requests {
        assert_eq!(request.tls_server_name(), request.url_host());
        assert_eq!(request.certificate_name(), request.url_host());
        assert_eq!(request.host_header(), request.url_host());
    }
    assert_eq!(*dns.calls.lock().unwrap(), vec![HOST, CDN]);
}

#[test]
fn redirect_rebinding_and_transport_identity_mismatch_are_rejected() {
    let dns = FixtureDns::new(&[(HOST, &[IP_A]), (CDN, &[IP_B])]);
    let connector = ScriptedConnector {
        steps: vec![
            Step::Reply(
                ResponseHead::redirect(302, "https://cdn.example/file.nzb"),
                vec![],
            ),
            Step::Fail(ConnectorError::IdentityMismatch),
        ],
        ..Default::default()
    };
    let mut client = transport(connector);
    let cancel = Cancellation::new();
    assert_eq!(
        fetch_nzb_streaming(START, &config(), &dns, &mut client, &cancel),
        Err(FetchRejection::IdentityMismatch),
    );
    assert_eq!(*dns.calls.lock().unwrap(), vec![HOST, CDN]);

    let rebinding = FixtureDns::new(&[(HOST, &[IP_A]), (CDN, &[IP_A, IP_B])]);
    let mut client = transport(ScriptedConnector {
        steps: vec![Step::Reply(
            ResponseHead::redirect(302, "https://cdn.example/file.nzb"),
            vec![],
        )],
        ..Default::default()
    });
    assert_eq!(
        fetch_nzb_streaming(
            START,
            &config(),
            &rebinding,
            &mut client,
            &Cancellation::new()
        ),
        Err(FetchRejection::ForbiddenAddress),
    );
}

#[test]
fn streamed_body_limit_is_checked_before_excess_bytes_are_buffered() {
    let dns = FixtureDns::new(&[(HOST, &[IP_A])]);
    let connector = ScriptedConnector {
        steps: vec![Step::Reply(
            ResponseHead::ok(200),
            vec![Ok(b"12345678".to_vec()), Ok(b"123456789".to_vec())],
        )],
        ..Default::default()
    };
    let mut client = transport(connector);
    let error =
        fetch_nzb_streaming(START, &config(), &dns, &mut client, &Cancellation::new()).unwrap_err();
    assert_eq!(error, FetchRejection::TooLarge);
    assert_eq!(
        client.buffered_body_bytes(),
        8,
        "oversized chunk must be rejected before append"
    );
}

#[test]
fn redirect_and_error_bodies_are_capped_and_cancellation_stops_streaming() {
    let dns = FixtureDns::new(&[(HOST, &[IP_A])]);
    let mut redirect = transport(ScriptedConnector {
        steps: vec![Step::Reply(
            ResponseHead::redirect(302, "https://nzb.example/end"),
            vec![Ok(vec![b'x'; 129])],
        )],
        ..Default::default()
    });
    assert_eq!(
        fetch_nzb_streaming(START, &config(), &dns, &mut redirect, &Cancellation::new()),
        Err(FetchRejection::TooLarge)
    );

    let mut error = transport(ScriptedConnector {
        steps: vec![Step::Reply(
            ResponseHead::status(502),
            vec![Ok(vec![b'x'; 129])],
        )],
        ..Default::default()
    });
    assert_eq!(
        fetch_nzb_streaming(START, &config(), &dns, &mut error, &Cancellation::new()),
        Err(FetchRejection::TooLarge)
    );

    let cancellation = Cancellation::new();
    let mut cancelled = transport(ScriptedConnector {
        steps: vec![Step::Reply(
            ResponseHead::ok(200),
            vec![
                Ok(b"first".to_vec()),
                Err(ConnectorError::Cancelled),
                Ok(b"never-read".to_vec()),
            ],
        )],
        ..Default::default()
    });
    assert_eq!(
        fetch_nzb_streaming(START, &config(), &dns, &mut cancelled, &cancellation),
        Err(FetchRejection::Cancelled)
    );
    assert_eq!(cancelled.stream_chunks_read(), 2);
}

#[test]
fn one_monotonic_deadline_covers_connect_headers_body_and_redirects() {
    let dns = FixtureDns::new(&[(HOST, &[IP_A])]);
    for failure in [
        ConnectorError::ConnectTimeout,
        ConnectorError::HeadersTimeout,
    ] {
        let mut client = transport(ScriptedConnector {
            steps: vec![Step::Fail(failure)],
            ..Default::default()
        });
        assert_eq!(
            fetch_nzb_streaming(START, &config(), &dns, &mut client, &Cancellation::new()),
            Err(FetchRejection::Timeout)
        );
    }
    let mut client = transport(ScriptedConnector {
        steps: vec![
            Step::Delayed(
                Duration::from_millis(900),
                ResponseHead::redirect(302, "https://nzb.example/next"),
                vec![],
            ),
            Step::Delayed(
                Duration::from_millis(1_200),
                ResponseHead::ok(200),
                vec![Ok(b"late".to_vec())],
            ),
        ],
        ..Default::default()
    });
    assert_eq!(
        fetch_nzb_streaming(START, &config(), &dns, &mut client, &Cancellation::new()),
        Err(FetchRejection::Timeout)
    );
}

#[test]
fn rejection_events_expose_only_sanitized_host_and_stable_code() {
    let dns = FixtureDns::new(&[(HOST, &[IP_A])]);
    let mut client = transport(ScriptedConnector {
        steps: vec![Step::Fail(ConnectorError::ConnectTimeout)],
        ..Default::default()
    });
    let result = fetch_nzb_streaming(START, &config(), &dns, &mut client, &Cancellation::new());
    assert_eq!(result, Err(FetchRejection::Timeout));
    let event = client.last_rejection_event().expect("rejection event");
    assert_eq!(event.host(), HOST);
    assert_eq!(event.code(), "nzb_fetch_timeout");
    let rendered = event.to_string();
    for secret in [START, "/start", "token=private", "private"] {
        assert!(
            !rendered.contains(secret),
            "rejection event leaked {secret:?}: {rendered}"
        );
    }
}

#[test]
fn allowlist_defaults_to_deny_all_and_accepts_only_explicit_hosts() {
    assert!(FetchConfig::default().allowed_hosts.is_empty());
    assert_eq!(
        FetchConfig::allowlist_from_env(" nzb.example ,, cdn.example "),
        vec![HOST.to_owned(), CDN.to_owned()]
    );
}
