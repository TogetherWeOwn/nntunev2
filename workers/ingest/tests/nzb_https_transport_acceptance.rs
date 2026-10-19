//! Tests-only RED handoff: asynchronous NZB HTTPS transport, NOT route wiring.
//!
//! # Required production seam (currently absent)
//!
//! `nzb_fetch::https_transport` must provide the imports below. The old synchronous
//! `DnsResolver`, `FetchTransport`, `Connector::connect` and iterator `StreamChunk`
//! cannot interrupt pending Workers I/O and are not adapters for this contract.
//!
//! The wished-for API is deliberately below the production driver/parser:
//!
//! - `AsyncResolver::start_resolve(host) -> Box<dyn DnsQuery>` starts one lookup.
//!   `DnsQuery::poll_answer(cx, remaining)` is asynchronous; `abort()` must release
//!   the actual pending query, not merely drop its future.
//! - `Connector::start_connect(ConnectRequest) -> Box<dyn Connection>` starts one
//!   numeric-address dial from the already-vetted answer set. Request getters:
//!   `addresses()`, `port()`, `tls_server_name()`, `certificate_name()`. No hidden
//!   DNS lookup, redirect following, or hostname-to-address fallback is allowed.
//! - `Connection` has `poll_connected`, `poll_tls`, `poll_write`, `poll_read`,
//!   `poll_close` and `abort`, with signatures implemented in the local fixture.
//!   Every poll receives the current remaining monotonic budget. `poll_write`
//!   and `poll_read` expose RAW HTTP bytes: production owns framing, partial
//!   writes, bounded headers, redirect decisions, body caps and phase ordering.
//! - `abort()` must synchronously force raw-resource release even while TLS is
//!   pending. It must not await graceful upgraded-socket `close()`. If Workers
//!   cannot implement that promise, this transport remains unavailable; racing
//!   I/O, dropping a future or detaching an unclosed socket does NOT satisfy it.
//! - `Clock::{now,sleep_until}` uses monotonic absolute time and an actual wakeable
//!   timer future. `Cancellation::{new,cancel}` wakes the production future.
//! - `BodySink::append` receives success-body bytes only AFTER the next append
//!   has been checked against the limit. On error the caller discards this
//!   provisional sink; partial data must never be committed to NZB intake.
//! - `DiagnosticSink::rejected(host, stable_code)` receives sanitized host + code,
//!   never path/query/credentials, upstream bodies or runtime error strings.
//! - `HttpsTransport::new(connector, clock, TransportConfig, diagnostics)` and
//!   async `fetch_nzb_streaming(url, policy, &mut resolver, &mut transport,
//!   &cancellation, &mut sink) -> Result<(), FetchRejection>` drive the REAL
//!   transport. See `transport_config()` for configuration fields. Enforce one
//!   deadline from before DNS through cleanup across all phases and redirects;
//!   at exact expiry timeout wins over simultaneously-ready I/O. No fresh budget.
//!
//! Bounds include header status line, names, values, separators and Location.
//! Read no more than the remaining cap + ONE overflow-detection byte; never
//! append that byte. Redirect/error bodies get their own cumulative caps (not
//! the NZB body cap); dispose the previous connection before resolving a hop.
//! Header and body overflow map to TooLarge. IdentityMismatch and Cancelled
//! extend the policy rejection taxonomy without weakening any address guard.
//!
//! # Evidence boundary / must-catch counterexample
//!
//! These tests poll the production future while lower-level I/O is pending,
//! advance a wakeable test clock OUTSIDE that future, then inspect independent
//! fixture resources. Neither supplied elapsed values nor injected errors prove
//! interruption. Fixture Drop intentionally leaves resources live. Graceful
//! close can stay pending forever. No URL-only or buffered-response fake is used.
//!
//! The local workerd 2026-10-12 probe took 2,002 ms on a 100 ms TLS-stall deadline
//! when awaiting upgraded-socket close (raw TCP/body cancellation ~102 ms). The
//! TLS tests below require force release and settlement at 100 ms WITHOUT opening
//! the close gate. Waiting 2 seconds, returning without release, or dropping the
//! socket fails. A separate test retains that regression with late TLS readiness.
//!
//! Passing deterministic seam tests is NOT native Workers runtime/TLS evidence.
//! A Builder must additionally prove the concrete adapter with local runtime
//! fixtures: peer-observed teardown and correct/wrong-name/untrusted-cert TLS.
//! An `unsupported expectedServerHostname option` diagnostic is not a supported
//! security boundary. No unsupported override, live DNS/network, staging,
//! production, credentials, dependencies, environment reader or route is added
//! here. URL fetching stays unavailable until those separate gates are met.
//!
//! This target is expected to fail to compile at the missing async module on the
//! assigned base. That is a compile-RED API handoff, NOT executed runtime tests.
//! The Builder implements this seam without editing either author-owned file.

use std::net::{IpAddr, Ipv4Addr};

use nntune_ingest::nzb_fetch::https_transport::{
    fetch_nzb_streaming, Cancellation, ConnectorError, HttpsTransport, TransportConfig,
};
use nntune_ingest::nzb_fetch::{FetchConfig, FetchRejection};

#[path = "support/nzb_https_async.rs"]
mod fixture;
use fixture::*;

const HOST: &str = "nzb.example";
const CDN: &str = "cdn.example";
// Synthetic fake-DNS answers ONLY. No fixture ever dials these public addresses.
const IP_A: IpAddr = IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34));
const IP_B: IpAddr = IpAddr::V4(Ipv4Addr::new(93, 184, 216, 35));
const DOCUMENTATION: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 18));
const PRIVATE: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3));
const START: &str = "https://nzb.example/start?token=private";

type Client = HttpsTransport<FixtureConnector, ManualClock, Diagnostics>;

fn policy() -> FetchConfig {
    FetchConfig {
        allowed_hosts: vec![HOST.into(), CDN.into()],
        timeout_ms: 100,
        max_redirects: 3,
        max_bytes: 16,
    }
}

fn transport_config() -> TransportConfig {
    TransportConfig {
        timeout: ms(100),
        max_redirects: 3,
        max_bytes: 16,
        max_header_bytes: 256,
        max_redirect_body_bytes: 128,
        max_error_body_bytes: 128,
    }
}

struct Rig {
    client: Client,
    dns: FixtureDns,
    probe: Probe,
    clock: ManualClock,
    diagnostics: Diagnostics,
}

fn rig(hops: &[Hop], answers: &[Answer]) -> Rig {
    let probe = Probe::default();
    let clock = ManualClock::default();
    // Nonzero epoch catches absolute-time/reset-to-zero errors.
    clock.advance(ms(1_234));
    let diagnostics = Diagnostics::default();
    Rig {
        client: HttpsTransport::new(
            FixtureConnector::new(hops, probe.clone()),
            clock.clone(),
            transport_config(),
            diagnostics.clone(),
        ),
        dns: FixtureDns::new(answers, probe.clone()),
        probe,
        clock,
        diagnostics,
    }
}

fn immediate(
    url: &str,
    cfg: &FetchConfig,
    hops: &[Hop],
    answers: &[Answer],
) -> (Result<(), FetchRejection>, Sink, Probe, Diagnostics) {
    let Rig {
        mut client,
        mut dns,
        clock,
        probe,
        diagnostics,
    } = rig(hops, answers);
    let cancel = Cancellation::new();
    let mut sink = Sink::default();
    let mut fetch = Box::pin(fetch_nzb_streaming(
        url,
        cfg,
        &mut dns,
        &mut client,
        &cancel,
        &mut sink,
    ));
    let result = Executor::default().ready(fetch.as_mut());
    drop(fetch);
    assert_eq!(
        clock.active_alarms(),
        0,
        "terminal result leaked a deadline timer"
    );
    assert!(
        hops.iter().all(|h| !h.live()),
        "result preceded raw-socket release"
    );
    assert!(
        answers.iter().all(|a| !a.live()),
        "result preceded DNS teardown"
    );
    (result, sink, probe, diagnostics)
}

fn written(probe: &Probe, id: usize) -> String {
    String::from_utf8(
        probe
            .events()
            .into_iter()
            .filter_map(|event| match event {
                Event::Write(i, bytes) if i == id => Some(bytes),
                _ => None,
            })
            .flatten()
            .collect(),
    )
    .unwrap()
}

fn assert_released_before_next_dns(probe: &Probe, socket: usize, next_dns: usize) {
    let events = probe.events();
    let released = events
        .iter()
        .position(|e| *e == Event::Released(socket))
        .expect("old raw connection released");
    let lookup = events
        .iter()
        .position(|e| matches!(e, Event::Resolve(id, _) if *id == next_dns))
        .expect("next lookup");
    assert!(
        released < lookup,
        "next hop started before previous teardown: {events:?}"
    );
}

// Break caught: abandoning an operation future without force-aborting its raw
// resource, resetting a phase deadline, or awaiting stalled TLS graceful close.
#[test]
fn pending_io_in_every_phase_expires_and_releases_before_return() {
    for phase in IO_PHASES {
        let mut answer = Answer::new(HOST, &[IP_A]);
        let mut hop = Hop::ok(b"<nzb/>");
        if phase == Phase::Dns {
            answer.gate = Gate::default();
        } else {
            hop.stall(phase);
        }
        hop.stall(Phase::Close); // native upgraded-socket close is uncooperative
        let Rig {
            mut client,
            mut dns,
            clock,
            probe,
            diagnostics,
        } = rig(&[hop.clone()], &[answer.clone()]);
        let cancel = Cancellation::new();
        let cfg = policy();
        let mut sink = Sink::default();
        let executor = Executor::default();
        let mut fetch = Box::pin(fetch_nzb_streaming(
            START,
            &cfg,
            &mut dns,
            &mut client,
            &cancel,
            &mut sink,
        ));
        executor.pending(fetch.as_mut());
        assert_eq!(probe.starts().last().unwrap().1, phase);
        let starts = probe.starts();
        clock.advance(ms(99));
        executor.pending(fetch.as_mut());
        let wakes = executor.wakes();
        clock.advance(ms(1));
        assert!(
            executor.wakes() > wakes,
            "deadline did not wake stalled {phase:?}"
        );
        assert_eq!(
            executor.ready(fetch.as_mut()),
            Err(FetchRejection::Timeout),
            "phase {phase:?}"
        );
        assert!(!answer.live(), "DNS was only dropped at {phase:?}");
        assert!(!hop.live(), "socket was only dropped at {phase:?}");
        if phase == Phase::Dns {
            assert!(probe.events().contains(&Event::DnsAbort(0)));
        } else {
            assert!(probe.events().contains(&Event::SocketAbort(0)));
        }
        assert_eq!(
            probe
                .starts()
                .iter()
                .filter(|(_, p, _)| *p != Phase::Close)
                .cloned()
                .collect::<Vec<_>>(),
            starts
        );
        // Late I/O cannot resurrect the completed fetch or start another phase.
        answer.gate.release();
        if phase != Phase::Dns {
            hop.gate(phase).release();
        }
        clock.advance(ms(2_000));
        assert_eq!(
            probe
                .starts()
                .iter()
                .filter(|(_, p, _)| *p != Phase::Close)
                .cloned()
                .collect::<Vec<_>>(),
            starts
        );
        drop(fetch);
        assert_eq!(clock.active_alarms(), 0);
        assert_eq!(
            *diagnostics.0.lock().unwrap(),
            vec![(HOST.into(), "nzb_fetch_timeout".into())]
        );
    }
}

// Break caught: cancellation is only error mapping, or does not wake/tear down
// DNS, TCP, TLS, request writes, header waits and established-body waits.
#[test]
fn external_cancellation_interrupts_each_pending_phase_without_clock_advance() {
    for phase in IO_PHASES {
        let mut answer = Answer::new(HOST, &[IP_A]);
        let mut hop = Hop::ok(b"<nzb/>");
        if phase == Phase::Dns {
            answer.gate = Gate::default();
        } else {
            hop.stall(phase);
        }
        hop.stall(Phase::Close);
        let Rig {
            mut client,
            mut dns,
            clock,
            probe,
            diagnostics,
        } = rig(&[hop.clone()], &[answer.clone()]);
        let cfg = policy();
        let cancel = Cancellation::new();
        let mut sink = Sink::default();
        let executor = Executor::default();
        let mut fetch = Box::pin(fetch_nzb_streaming(
            START,
            &cfg,
            &mut dns,
            &mut client,
            &cancel,
            &mut sink,
        ));
        executor.pending(fetch.as_mut());
        assert_eq!(probe.starts().last().unwrap().1, phase);
        let starts = probe.starts();
        let wakes = executor.wakes();
        cancel.cancel();
        assert!(executor.wakes() > wakes, "cancel did not wake {phase:?}");
        assert_eq!(
            executor.ready(fetch.as_mut()),
            Err(FetchRejection::Cancelled)
        );
        assert!(!answer.live());
        assert!(
            !hop.live(),
            "cancel did not force raw teardown at {phase:?}"
        );
        assert_eq!(
            probe
                .starts()
                .iter()
                .filter(|(_, p, _)| *p != Phase::Close)
                .cloned()
                .collect::<Vec<_>>(),
            starts
        );
        drop(fetch);
        assert_eq!(clock.active_alarms(), 0);
        assert_eq!(
            *diagnostics.0.lock().unwrap(),
            vec![(HOST.into(), "nzb_fetch_cancelled".into())]
        );
    }
}

// Break caught: handling TLS cancellation by awaiting close (2,002 ms in the
// native counterexample), accepting late TLS success, or leaving a timer alive.
#[test]
fn tls_stall_cannot_wait_for_close_or_accept_simultaneous_late_handshake() {
    for late_ready in [false, true] {
        let mut hop = Hop::ok(b"<nzb/>");
        hop.stall(Phase::Tls);
        hop.stall(Phase::Close);
        let Rig {
            mut client,
            mut dns,
            clock,
            probe,
            ..
        } = rig(&[hop.clone()], &[Answer::new(HOST, &[IP_A])]);
        let cfg = policy();
        let cancel = Cancellation::new();
        let mut sink = Sink::default();
        let executor = Executor::default();
        let mut fetch = Box::pin(fetch_nzb_streaming(
            START,
            &cfg,
            &mut dns,
            &mut client,
            &cancel,
            &mut sink,
        ));
        executor.pending(fetch.as_mut());
        assert!(hop.live());
        assert_eq!(probe.starts().last().unwrap().1, Phase::Tls);
        clock.advance(ms(100));
        if late_ready {
            hop.gate(Phase::Tls).release();
        }
        assert_eq!(executor.ready(fetch.as_mut()), Err(FetchRejection::Timeout));
        assert!(!hop.live(), "settlement alone is not teardown evidence");
        assert!(probe.events().contains(&Event::SocketAbort(0)));
        assert!(
            written(&probe, 0).is_empty(),
            "late handshake emitted HTTP after expiry"
        );
        assert_eq!(probe.bytes_read(0, Phase::Body), 0);
        drop(fetch);
        assert!(sink.bytes.is_empty());
        assert_eq!(clock.active_alarms(), 0);
    }
}

// Break caught: fresh per-phase/per-hop timeouts; DNS omitted from the budget;
// passing the configured duration instead of monotonic remaining time to I/O.
#[test]
fn remaining_budget_decreases_across_dns_tls_request_headers_body_and_redirects() {
    let mut first = Hop::redirect("https://cdn.example/final", &[b"x"]);
    let mut second = Hop::ok(b"<nzb/>");
    for phase in &IO_PHASES[1..] {
        first.stall(*phase);
        second.stall(*phase);
    }
    second.stall(Phase::Close);
    let mut a = Answer::new(HOST, &[IP_A]);
    let mut b = Answer::new(CDN, &[IP_B]);
    a.gate = Gate::default();
    b.gate = Gate::default();
    let Rig {
        mut client,
        mut dns,
        clock,
        probe,
        ..
    } = rig(&[first.clone(), second.clone()], &[a.clone(), b.clone()]);
    let cfg = policy();
    let cancel = Cancellation::new();
    let mut sink = Sink::default();
    let executor = Executor::default();
    let mut fetch = Box::pin(fetch_nzb_streaming(
        START,
        &cfg,
        &mut dns,
        &mut client,
        &cancel,
        &mut sink,
    ));
    executor.pending(fetch.as_mut());
    for (elapsed, gate, next) in [
        (10, a.gate.clone(), Phase::Connect),
        (15, first.gate(Phase::Connect), Phase::Tls),
        (20, first.gate(Phase::Tls), Phase::Write),
        (10, first.gate(Phase::Write), Phase::Headers),
        (10, first.gate(Phase::Headers), Phase::Body),
        (10, first.gate(Phase::Body), Phase::Dns),
        (5, b.gate.clone(), Phase::Connect),
        (5, second.gate(Phase::Connect), Phase::Tls),
        (5, second.gate(Phase::Tls), Phase::Write),
        (2, second.gate(Phase::Write), Phase::Headers),
        (2, second.gate(Phase::Headers), Phase::Body),
    ] {
        clock.advance(ms(elapsed));
        gate.release();
        executor.pending(fetch.as_mut());
        assert_eq!(probe.starts().last().unwrap().1, next);
    }
    assert_released_before_next_dns(&probe, 0, 1);
    assert_eq!(
        probe
            .starts()
            .into_iter()
            .filter(|(_, p, _)| *p != Phase::Close)
            .collect::<Vec<_>>(),
        vec![
            (0, Phase::Dns, ms(100)),
            (0, Phase::Connect, ms(90)),
            (0, Phase::Tls, ms(75)),
            (0, Phase::Write, ms(55)),
            (0, Phase::Headers, ms(45)),
            (0, Phase::Body, ms(35)),
            (1, Phase::Dns, ms(25)),
            (1, Phase::Connect, ms(20)),
            (1, Phase::Tls, ms(15)),
            (1, Phase::Write, ms(10)),
            (1, Phase::Headers, ms(8)),
            (1, Phase::Body, ms(6)),
        ]
    );
    clock.advance(ms(6));
    assert_eq!(executor.ready(fetch.as_mut()), Err(FetchRejection::Timeout));
    assert!(!first.live() && !second.live());
    drop(fetch);
    assert!(sink.bytes.is_empty(), "redirect bytes are not NZB bytes");
    assert_eq!(clock.active_alarms(), 0);
}

// Break caught: resetting the timer after each body chunk, forgetting the
// deadline/cancellation while draining non-success bodies, or reading after abort.
#[test]
fn partial_success_redirect_and_error_bodies_do_not_reset_or_escape_the_deadline() {
    for status in [200, 302, 502] {
        for cancelled in [false, true] {
            let head = match status {
                200 => "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n",
                302 => "HTTP/1.1 302 Found\r\nLocation: https://cdn.example/end\r\nConnection: close\r\n\r\n",
                _ => "HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n",
            };
            let mut hop = Hop::raw(head, &[b"12345678", b"NEVER"]);
            hop.body_gates = vec![Gate::default(), Gate::default()];
            hop.stall(Phase::Close);
            let Rig {
                mut client,
                mut dns,
                clock,
                probe,
                ..
            } = rig(&[hop.clone()], &[Answer::new(HOST, &[IP_A])]);
            let cfg = policy();
            let cancel = Cancellation::new();
            let mut sink = Sink::default();
            let executor = Executor::default();
            let mut fetch = Box::pin(fetch_nzb_streaming(
                START,
                &cfg,
                &mut dns,
                &mut client,
                &cancel,
                &mut sink,
            ));
            executor.pending(fetch.as_mut());
            clock.advance(ms(40));
            hop.body_gates[0].release();
            executor.pending(fetch.as_mut());
            assert_eq!(probe.bytes_read(0, Phase::Body), 8);
            assert_eq!(probe.starts().last().unwrap(), &(0, Phase::Body, ms(60)));
            let expected = if cancelled {
                let wakes = executor.wakes();
                cancel.cancel();
                assert!(executor.wakes() > wakes);
                FetchRejection::Cancelled
            } else {
                clock.advance(ms(59));
                executor.pending(fetch.as_mut());
                clock.advance(ms(1));
                FetchRejection::Timeout
            };
            assert_eq!(executor.ready(fetch.as_mut()), Err(expected));
            assert!(!hop.live());
            assert!(probe.events().contains(&Event::SocketAbort(0)));
            assert_eq!(probe.bytes_read(0, Phase::Body), 8);
            assert_eq!(
                probe
                    .events()
                    .iter()
                    .filter(|e| matches!(e, Event::Resolve(..)))
                    .count(),
                1
            );
            drop(fetch);
            if status == 200 {
                assert_eq!(sink.bytes, b"12345678");
            } else {
                assert!(sink.bytes.is_empty());
            }
            assert_eq!(clock.active_alarms(), 0);
        }
    }
}

// Break caught: skipping validation on redirects, automatic redirect handling,
// hostname dialing/re-resolution, losing SNI/certificate identity, wrong Host,
// or failing to finish partial request writes.
#[test]
fn redirect_hops_pin_numeric_sets_and_keep_original_tls_and_http_identity() {
    let (result, sink, probe, diagnostics) = immediate(
        START,
        &policy(),
        &[
            Hop::redirect("https://cdn.example/file.nzb", &[]),
            Hop::ok(b"<nzb/>"),
        ],
        &[Answer::new(HOST, &[IP_A, IP_B]), Answer::new(CDN, &[IP_B])],
    );
    assert_eq!(result, Ok(()));
    assert_eq!(sink.bytes, b"<nzb/>");
    let dials = probe
        .events()
        .into_iter()
        .filter(|e| matches!(e, Event::Dial(..)))
        .collect::<Vec<_>>();
    assert_eq!(
        dials,
        vec![
            Event::Dial(0, vec![IP_A, IP_B], HOST.into(), HOST.into()),
            Event::Dial(1, vec![IP_B], CDN.into(), CDN.into()),
        ]
    );
    let lookups = probe
        .events()
        .into_iter()
        .filter(|e| matches!(e, Event::Resolve(..)))
        .collect::<Vec<_>>();
    assert_eq!(
        lookups,
        vec![
            Event::Resolve(0, HOST.into()),
            Event::Resolve(1, CDN.into())
        ]
    );
    for (id, host, target) in [(0, HOST, "/start?token=private"), (1, CDN, "/file.nzb")] {
        let request = written(&probe, id);
        assert!(request.starts_with(&format!("GET {target} HTTP/1.1\r\n")));
        let host_lines = request
            .lines()
            .filter(|line| line.to_ascii_lowercase().starts_with("host:"))
            .collect::<Vec<_>>();
        assert_eq!(host_lines.len(), 1);
        assert_eq!(host_lines[0].split_once(':').unwrap().1.trim(), host);
        assert!(request.ends_with("\r\n\r\n"));
    }
    assert_released_before_next_dns(&probe, 0, 1);
    assert!(diagnostics.0.lock().unwrap().is_empty());
}

// Break caught: accepting documentation answers or a mixed forbidden answer;
// treating a previously allowed host as permanently trusted after a redirect.
#[test]
fn documentation_and_mixed_forbidden_dns_answers_never_reach_the_connector() {
    for addresses in [
        vec![DOCUMENTATION],
        vec![IP_B, DOCUMENTATION],
        vec![IP_A, PRIVATE],
    ] {
        let (result, sink, probe, _) =
            immediate(START, &policy(), &[], &[Answer::new(HOST, &addresses)]);
        assert_eq!(result, Err(FetchRejection::ForbiddenAddress));
        assert!(sink.bytes.is_empty());
        assert!(!probe.events().iter().any(|e| matches!(e, Event::Dial(..))));

        let (result, _, probe, _) = immediate(
            START,
            &policy(),
            &[Hop::redirect("https://cdn.example/file.nzb", &[])],
            &[Answer::new(HOST, &[IP_A]), Answer::new(CDN, &addresses)],
        );
        assert_eq!(result, Err(FetchRejection::ForbiddenAddress));
        assert_eq!(
            probe
                .events()
                .iter()
                .filter(|e| matches!(e, Event::Dial(..)))
                .count(),
            1
        );
        assert_released_before_next_dns(&probe, 0, 1);
    }
    let (result, _, probe, _) = immediate(
        START,
        &policy(),
        &[Hop::redirect("https://nzb.example/next", &[])],
        &[Answer::new(HOST, &[IP_A]), Answer::new(HOST, &[PRIVATE])],
    );
    assert_eq!(result, Err(FetchRejection::ForbiddenAddress));
    assert_eq!(
        probe
            .events()
            .iter()
            .filter(|e| matches!(e, Event::Dial(..)))
            .count(),
        1
    );
}

// Break caught: append-then-check, wrong cumulative cap, reading trailing bytes
// after overflow, or rejecting the exact permitted byte boundary.
#[test]
fn body_overflow_is_rejected_before_sink_append_and_stops_reading() {
    let hop = Hop::raw(
        "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n",
        &[b"12345678", b"123456789", b"NEVER"],
    );
    let (result, sink, probe, _) =
        immediate(START, &policy(), &[hop], &[Answer::new(HOST, &[IP_A])]);
    assert_eq!(result, Err(FetchRejection::TooLarge));
    assert!(
        sink.bytes.len() <= 16,
        "oversized bytes reached the actual sink"
    );
    let mut appended = 0;
    for chunk in sink.appends {
        appended += chunk.len();
        assert!(appended <= 16);
    }
    assert_eq!(
        probe.bytes_read(0, Phase::Body),
        17,
        "only one detection byte beyond the cap"
    );
    let (result, sink, _, _) = immediate(
        START,
        &policy(),
        &[Hop::ok(b"1234567890123456")],
        &[Answer::new(HOST, &[IP_A])],
    );
    assert_eq!(result, Ok(()));
    assert_eq!(sink.bytes, b"1234567890123456");
}

// Break caught: unbounded header/Location buffering, bounding each line instead
// of the aggregate, or starting a body/redirect after the header cap is exceeded.
#[test]
fn aggregate_headers_and_location_are_bounded_before_body_or_redirect() {
    for head in [
        format!("HTTP/1.1 200 OK\r\nX-Fill: {}\r\n\r\n", "x".repeat(300)),
        format!("HTTP/1.1 200 OK\r\n{}\r\n", "X: y\r\n".repeat(60)),
        format!(
            "HTTP/1.1 302 Found\r\nLocation: https://cdn.example/{}\r\n\r\n",
            "x".repeat(300)
        ),
    ] {
        let (result, sink, probe, _) = immediate(
            START,
            &policy(),
            &[Hop::raw(&head, &[b"NEVER"])],
            &[Answer::new(HOST, &[IP_A])],
        );
        assert_eq!(result, Err(FetchRejection::TooLarge));
        assert!(sink.bytes.is_empty());
        assert!(
            probe.bytes_read(0, Phase::Headers) <= 257,
            "unbounded header read"
        );
        assert_eq!(probe.bytes_read(0, Phase::Body), 0);
        assert_eq!(
            probe
                .events()
                .iter()
                .filter(|e| matches!(e, Event::Resolve(..)))
                .count(),
            1
        );
    }
    let prefix = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nX-Fill: ";
    let head = format!("{prefix}{}\r\n\r\n", "x".repeat(256 - prefix.len() - 4));
    assert_eq!(head.len(), 256);
    let (result, sink, _, _) = immediate(
        START,
        &policy(),
        &[Hop::raw(&head, &[])],
        &[Answer::new(HOST, &[IP_A])],
    );
    assert_eq!(result, Ok(()), "exact header cap must be accepted");
    assert!(sink.bytes.is_empty());
}

// Break caught: draining redirect/error bodies without a cap, applying the NZB
// cap to these bodies, or appending non-success bytes to the NZB sink.
#[test]
fn redirect_and_error_bodies_have_independent_cumulative_bounds() {
    for redirect in [false, true] {
        let chunks: &[&[u8]] = &[&[b'x'; 64], &[b'x'; 65], b"NEVER"];
        let hop = if redirect {
            Hop::redirect("https://cdn.example/end", chunks)
        } else {
            Hop::raw(
                "HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n",
                chunks,
            )
        };
        let (result, sink, probe, _) =
            immediate(START, &policy(), &[hop], &[Answer::new(HOST, &[IP_A])]);
        assert_eq!(result, Err(FetchRejection::TooLarge));
        assert!(sink.bytes.is_empty());
        assert_eq!(probe.bytes_read(0, Phase::Body), 129);
        assert_eq!(
            probe
                .events()
                .iter()
                .filter(|e| matches!(e, Event::Resolve(..)))
                .count(),
            1
        );
    }
    let bytes = [b'x'; 128];
    let (result, sink, _, _) = immediate(
        START,
        &policy(),
        &[Hop::raw(
            "HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n",
            &[&bytes],
        )],
        &[Answer::new(HOST, &[IP_A])],
    );
    assert_eq!(result, Err(FetchRejection::UpstreamStatus(502)));
    assert!(sink.bytes.is_empty());
    let (result, sink, probe, _) = immediate(
        START,
        &policy(),
        &[
            Hop::redirect("https://cdn.example/end", &[&bytes]),
            Hop::ok(b"<nzb/>"),
        ],
        &[Answer::new(HOST, &[IP_A]), Answer::new(CDN, &[IP_B])],
    );
    assert_eq!(result, Ok(()));
    assert_eq!(sink.bytes, b"<nzb/>");
    assert_released_before_next_dns(&probe, 0, 1);
}

// Break caught: redirect loops, http downgrade, credentials or non-allowlisted
// redirect targets bypassing the per-hop policy. No live route is exercised.
#[test]
fn redirects_remain_manual_capped_and_deny_by_policy() {
    for (location, expected) in [
        ("http://cdn.example/end", FetchRejection::BadScheme),
        (
            "https://user:SECRET@cdn.example/end",
            FetchRejection::EmbeddedCredentials,
        ),
        (
            "https://evil.example/end",
            FetchRejection::HostNotAllowlisted,
        ),
    ] {
        let (result, _, probe, _) = immediate(
            START,
            &policy(),
            &[Hop::redirect(location, &[])],
            &[Answer::new(HOST, &[IP_A])],
        );
        assert_eq!(result, Err(expected));
        assert_eq!(
            probe
                .events()
                .iter()
                .filter(|e| matches!(e, Event::Dial(..)))
                .count(),
            1
        );
    }
    let hops = (0..4)
        .map(|_| Hop::redirect(START, &[]))
        .collect::<Vec<_>>();
    let answers = (0..4)
        .map(|_| Answer::new(HOST, &[IP_A]))
        .collect::<Vec<_>>();
    let (result, sink, probe, _) = immediate(START, &policy(), &hops, &answers);
    assert_eq!(result, Err(FetchRejection::TooManyRedirects));
    assert!(sink.bytes.is_empty());
    assert_eq!(
        probe
            .events()
            .iter()
            .filter(|e| matches!(e, Event::Dial(..)))
            .count(),
        4
    );
}

// MAPPING ONLY: injected timeout/cancellation/identity errors are NOT evidence
// that an actual pending operation can be interrupted or TLS can verify peers.
#[test]
fn injected_transport_errors_retain_stable_rejection_mapping_only() {
    for (error, expected, code) in [
        (
            ConnectorError::ConnectTimeout,
            FetchRejection::Timeout,
            "nzb_fetch_timeout",
        ),
        (
            ConnectorError::HeadersTimeout,
            FetchRejection::Timeout,
            "nzb_fetch_timeout",
        ),
        (
            ConnectorError::Cancelled,
            FetchRejection::Cancelled,
            "nzb_fetch_cancelled",
        ),
        (
            ConnectorError::IdentityMismatch,
            FetchRejection::IdentityMismatch,
            "nzb_fetch_identity_mismatch",
        ),
    ] {
        let mut hop = Hop::ok(b"<nzb/>");
        match error {
            ConnectorError::IdentityMismatch => hop.tls_error = Some(error),
            ConnectorError::HeadersTimeout | ConnectorError::Cancelled => {
                hop.read_error = Some(error)
            }
            _ => hop.connect_error = Some(error),
        }
        let (result, sink, probe, diagnostics) =
            immediate(START, &policy(), &[hop], &[Answer::new(HOST, &[IP_A])]);
        assert_eq!(result, Err(expected));
        assert_eq!(
            *diagnostics.0.lock().unwrap(),
            vec![(HOST.into(), code.into())]
        );
        assert!(sink.bytes.is_empty());
        if expected == FetchRejection::IdentityMismatch {
            assert!(
                written(&probe, 0).is_empty(),
                "identity error must precede HTTP disclosure"
            );
        }
    }
}

// Break caught: diagnostics retaining a URL, upstream body, malformed input,
// credentials or query secrets, or naming the initial host on a rejected hop.
#[test]
fn diagnostics_contain_only_sanitized_rejected_host_and_stable_code() {
    for (url, expected_host, code) in [
        (
            "https://user:SECRET-PASS@nzb.example/path-secret?apikey=SECRET-KEY",
            HOST,
            "nzb_fetch_embedded_credentials",
        ),
        (
            "https://evil.example/path-secret?apikey=SECRET-KEY",
            "evil.example",
            "nzb_fetch_host_not_allowlisted",
        ),
    ] {
        let (result, _, _, diagnostics) = immediate(url, &policy(), &[], &[]);
        assert!(result.is_err());
        assert_eq!(
            *diagnostics.0.lock().unwrap(),
            vec![(expected_host.into(), code.into())]
        );
    }
    let (result, _, _, diagnostics) = immediate(
        START,
        &policy(),
        &[Hop::redirect(
            "https://cdn.example/path-secret?apikey=SECRET-KEY",
            &[],
        )],
        &[
            Answer::new(HOST, &[IP_A]),
            Answer::new(CDN, &[DOCUMENTATION]),
        ],
    );
    assert_eq!(result, Err(FetchRejection::ForbiddenAddress));
    assert_eq!(
        *diagnostics.0.lock().unwrap(),
        vec![(CDN.into(), "nzb_fetch_forbidden_address".into())]
    );
    let (result, _, _, diagnostics) = immediate("not a url SECRET-KEY", &policy(), &[], &[]);
    assert_eq!(result, Err(FetchRejection::MalformedUrl));
    let events = diagnostics.0.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1, "nzb_fetch_malformed_url");
    for secret in [
        "SECRET-KEY",
        "not a url",
        "token=private",
        "/path-secret",
        "apikey=",
    ] {
        assert!(!format!("{events:?}").contains(secret));
    }
}

// MODEL/POLICY ONLY: supplied allowlist parsing is not environment wiring.
#[test]
fn allowlist_defaults_to_deny_all_before_any_async_io() {
    assert!(FetchConfig::default().allowed_hosts.is_empty());
    assert_eq!(
        FetchConfig::allowlist_from_env(" nzb.example ,, cdn.example "),
        vec![HOST.to_owned(), CDN.to_owned()]
    );
    let (result, sink, probe, _) = immediate(START, &FetchConfig::default(), &[], &[]);
    assert_eq!(result, Err(FetchRejection::HostNotAllowlisted));
    assert!(sink.bytes.is_empty());
    assert!(probe.starts().is_empty());
    assert!(!probe
        .events()
        .iter()
        .any(|e| matches!(e, Event::Resolve(..) | Event::Dial(..))));
}

#[test]
fn precancelled_fetch_performs_no_dns_or_socket_io() {
    let Rig {
        mut client,
        mut dns,
        clock,
        probe,
        ..
    } = rig(&[], &[]);
    let cfg = policy();
    let cancel = Cancellation::new();
    cancel.cancel();
    let mut sink = Sink::default();
    let mut fetch = Box::pin(fetch_nzb_streaming(
        START,
        &cfg,
        &mut dns,
        &mut client,
        &cancel,
        &mut sink,
    ));
    assert_eq!(
        Executor::default().ready(fetch.as_mut()),
        Err(FetchRejection::Cancelled)
    );
    drop(fetch);
    assert!(probe.events().is_empty());
    assert!(sink.bytes.is_empty());
    assert_eq!(clock.active_alarms(), 0);
}
