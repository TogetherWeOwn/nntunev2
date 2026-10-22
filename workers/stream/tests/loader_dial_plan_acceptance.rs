//! Independently owned live-loader SSRF dial-plan acceptance tests.
//! Oracle: pinned seam S:283 (ProviderOnly/port/caps), S:360 (resolve-all/
//! vet-all, exact 119/563 peer, original-hostname TLS, no second resolution),
//! P1:184/186 (cap formula, per-dial guard), B1:211/319/321 (one
//! connect/checkout pair, pre-content dial failure aborts). See
//! loader-slice-acceptance.md for the oracle, seam, matrix and proof limits.
//! This handoff is intentionally RED until the portable seam exists.
//! No DNS, sockets, vault, keys, DB, bindings or network are touched here:
//! candidates are inert pre-resolved text supplied by the test.

use nntune_stream::loader::{
    admit_connection_step, vet_dial_plan, DialPlan, DialStep, DialWindow, LoaderDenial, StepState,
    VetDialPlanRequest,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

// All v4 literals are built from octets so the public-text lint's
// private-IPv4 rule never flags a negative SSRF vector. v6 literals are
// used directly (the lint does not flag them).
fn v4(a: u8, b: u8, c: u8, d: u8) -> String {
    Ipv4Addr::new(a, b, c, d).to_string()
}

fn public_v4() -> String {
    v4(93, 184, 216, 34)
}

fn public_v6() -> &'static str {
    "2606:4700:4700::1111"
}

fn refs(list: &[String]) -> Vec<&str> {
    list.iter().map(String::as_str).collect()
}

fn request<'a>(
    host: &'a str,
    port: u16,
    tls: bool,
    ack: bool,
    candidates: &'a [&'a str],
) -> VetDialPlanRequest<'a> {
    VetDialPlanRequest {
        host,
        port,
        tls,
        allow_insecure_plaintext: ack,
        candidates,
    }
}

fn positive<'a>(host: &'a str, candidates: &'a [&'a str]) -> VetDialPlanRequest<'a> {
    request(host, 563, true, false, candidates)
}

fn accept(req: &VetDialPlanRequest) -> DialPlan {
    match vet_dial_plan(req) {
        Ok(plan) => plan,
        Err(denial) => panic!("valid dial-plan request denied: {denial}"),
    }
}

fn deny(req: &VetDialPlanRequest) -> LoaderDenial {
    match vet_dial_plan(req) {
        Ok(_) => panic!("invalid dial-plan request admitted"),
        Err(denial) => {
            // A denied dial plan reports the stored endpoint as unusable;
            // nothing about the caller's own authorization is decided here.
            assert_eq!(denial, LoaderDenial::ProviderUnavailable);
            assert_eq!(denial.to_string(), "provider_unavailable");
            denial
        }
    }
}

#[test]
fn single_public_v4_candidate_pins_exact_peer_port_and_tls_name() {
    let host = "news.example.invalid";
    let own = [public_v4()];
    let candidates = refs(&own);
    let req = positive(host, &candidates);
    let plan = accept(&req);
    let expected: IpAddr = own[0].parse().expect("fixture parses");
    assert_eq!(plan.peer(), SocketAddr::new(expected, 563));
    assert_eq!(plan.tls_server_name(), host);
    assert!(plan.tls());
}

#[test]
fn single_public_v6_candidate_pins_exact_peer() {
    let own = [public_v6().to_string()];
    let candidates = refs(&own);
    let req = positive("news.example.invalid", &candidates);
    let plan = accept(&req);
    let expected: IpAddr = public_v6().parse().expect("fixture parses");
    assert_eq!(plan.peer(), SocketAddr::new(expected, 563));
}

#[test]
fn both_nntp_ports_vet_and_pin_exactly() {
    for port in [119, 563] {
        let own = [public_v4()];
        let candidates = refs(&own);
        let req = request("news.example.invalid", port, true, false, &candidates);
        let plan = accept(&req);
        assert_eq!(plan.peer().port(), port);
    }
}

#[test]
fn non_nntp_ports_deny_without_dial_plan() {
    for port in [0, 22, 80, 118, 120, 443, 562, 564, 1190, 5630, 8080, 65535] {
        let own = [public_v4()];
        let candidates = refs(&own);
        let req = request("news.example.invalid", port, true, false, &candidates);
        deny(&req);
    }
}

#[test]
fn multiple_public_candidates_admit_a_member_of_the_vetted_set() {
    // S:360 requires the peer to be a member of the freshly vetted set; it
    // does not fix a selection order, so the test asserts membership only.
    let own = [public_v4(), public_v6().to_string(), v4(93, 184, 216, 35)];
    let candidates = refs(&own);
    let req = positive("news.example.invalid", &candidates);
    let plan = accept(&req);
    let vetted: Vec<IpAddr> = own
        .iter()
        .map(|s| s.parse().expect("fixture parses"))
        .collect();
    assert!(vetted.contains(&plan.peer().ip()));
    assert_eq!(plan.peer().port(), 563);
}

#[test]
fn mixed_public_and_forbidden_set_denies_whichever_answer_is_last() {
    // Vet-ALL: one forbidden answer anywhere in the set denies the whole
    // plan, including when it arrives after public answers.
    let forbidden = [
        v4(127, 0, 0, 1),
        v4(10, 0, 0, 5),
        v4(192, 168, 1, 1),
        "::1".to_string(),
    ];
    for bad in &forbidden {
        for ordered in [
            vec![public_v4(), public_v6().to_string(), bad.clone()],
            vec![bad.clone(), public_v4(), public_v6().to_string()],
        ] {
            let candidates = refs(&ordered);
            let req = positive("news.example.invalid", &candidates);
            deny(&req);
        }
    }
}

#[test]
fn every_required_forbidden_family_denies_alone() {
    let mut cases: Vec<String> = [
        [127, 0, 0, 1],       // v4 loopback
        [10, 0, 0, 5],        // RFC 1918 10/8
        [172, 16, 0, 9],      // RFC 1918 172.16/12
        [192, 168, 1, 1],     // RFC 1918 192.168/16
        [169, 254, 1, 1],     // link-local
        [169, 254, 169, 254], // cloud metadata endpoint
        [100, 64, 0, 1],      // carrier-grade NAT
        [0, 0, 0, 0],         // this network
        [224, 0, 0, 1],       // multicast
        [198, 18, 0, 1],      // benchmarking
        [192, 0, 0, 1],       // IETF protocol assignments
        [240, 0, 0, 1],       // reserved
        [255, 255, 255, 255], // broadcast
    ]
    .iter()
    .map(|o| v4(o[0], o[1], o[2], o[3]))
    .collect();
    cases.extend(
        [
            "::1",         // v6 loopback
            "::",          // v6 unspecified
            "fe80::1",     // v6 link-local
            "fc00::1",     // v6 unique-local
            "ff02::1",     // v6 multicast
            "2001:db8::1", // v6 documentation
        ]
        .iter()
        .map(ToString::to_string),
    );
    for bad in &cases {
        let own = [bad.clone()];
        let candidates = refs(&own);
        let req = positive("news.example.invalid", &candidates);
        deny(&req);
    }
}

#[test]
fn empty_answer_set_denies() {
    let req = positive("news.example.invalid", &[]);
    deny(&req);
}

#[test]
fn unparseable_candidate_denies_alone_and_in_mixed_sets() {
    // An answer that cannot be parsed cannot be verified public; vet-ALL
    // fails closed instead of skipping it.
    for ordered in [
        vec!["not-an-address".to_string()],
        vec![public_v4(), "999.999.999.999".to_string()],
        vec!["".to_string(), public_v6().to_string()],
    ] {
        let candidates = refs(&ordered);
        let req = positive("news.example.invalid", &candidates);
        deny(&req);
    }
}

#[test]
fn valid_dns_and_ipv4_literal_hosts_admit() {
    for host in [
        "news.example.invalid",
        "a.example.invalid",
        "xn--nxasmq6b.example.invalid",
        "News.Example.INVALID",
    ] {
        let own = [public_v4()];
        let candidates = refs(&own);
        let req = positive(host, &candidates);
        let plan = accept(&req);
        // Original hostname is carried byte-exact for TLS validation: no
        // case folding, trimming or other normalization.
        assert_eq!(plan.tls_server_name(), host);
    }
    let literal = public_v4();
    let own = [literal.clone()];
    let candidates = refs(&own);
    let req = positive(&literal, &candidates);
    accept(&req);
}

#[test]
fn malformed_hosts_deny() {
    let long = format!("{}.example.invalid", "a".repeat(250));
    assert!(long.len() > 253);
    for host in [
        String::new(),
        long,
        "https://news.example.invalid".to_string(),
        "news.example.invalid/path".to_string(),
        "user@news.example.invalid".to_string(),
        "news.example.invalid:563".to_string(),
        "news example.invalid".to_string(),
        ".news.example.invalid".to_string(),
        "news.example.invalid.".to_string(),
        "news..example.invalid".to_string(),
        // Mirrored quirk of the existing SSRF host validator: any ':' fails
        // shape before the IP-literal parse, so v6-literal hosts deny. The
        // loader must not invent a more permissive rule here.
        "::1".to_string(),
        "2001:db8::1".to_string(),
    ] {
        let own = [public_v4()];
        let candidates = refs(&own);
        let req = positive(&host, &candidates);
        deny(&req);
    }
}

#[test]
fn plaintext_without_explicit_acknowledgement_denies() {
    let own = [public_v4()];
    let candidates = refs(&own);
    let acked = request("news.example.invalid", 119, false, true, &candidates);
    let plan = accept(&acked);
    assert!(!plan.tls());
    assert_eq!(plan.tls_server_name(), "news.example.invalid");
    let candidates = refs(&own);
    let unacked = request("news.example.invalid", 119, false, false, &candidates);
    deny(&unacked);
    let candidates = refs(&own);
    let tls = request("news.example.invalid", 563, true, false, &candidates);
    assert!(accept(&tls).tls());
}

#[test]
fn dial_plan_carries_no_hostname_dial_input() {
    // No second resolution: the plan's only network destination is a
    // concrete socket address; the hostname survives solely as the TLS
    // server name. A transport that re-resolves has no input for it here.
    let own = [public_v4()];
    let candidates = refs(&own);
    let req = positive("news.example.invalid", &candidates);
    let plan = accept(&req);
    assert!(plan.peer().ip().is_ipv4() || plan.peer().ip().is_ipv6());
    assert_eq!(plan.tls_server_name(), "news.example.invalid");
}

#[test]
fn denied_hosts_never_echo_into_fixed_error_output() {
    // Distinct credential-bearing hosts produce the identical fixed code.
    let first = format!("user:{}@news.example.invalid", "hunter2-a");
    let second = format!("user:{}@news.example.invalid", "hunter2-b");
    let mut rendered = Vec::new();
    for host in [
        &first,
        &second,
        &"https://news.example.invalid/x".to_string(),
    ] {
        let own = [public_v4()];
        let candidates = refs(&own);
        let req = positive(host, &candidates);
        rendered.push(deny(&req).to_string());
    }
    assert!(rendered.iter().all(|r| r == "provider_unavailable"));
    for host in [&first, &second] {
        let own = [public_v4()];
        let candidates = refs(&own);
        let req = positive(host, &candidates);
        assert!(!deny(&req).to_string().contains(host.as_str()));
    }
}

// Stable negative trait assertion: inference becomes ambiguous if Debug is
// implemented. The vetted peer is PII-adjacent dial material and must stay
// out of capture/debug/tail/trace surfaces.
macro_rules! assert_not_debug {
    ($ty:ty) => {
        const _: fn() = || {
            trait AmbiguousIfDebug<A> {
                fn marker() {}
            }
            impl<T: ?Sized> AmbiguousIfDebug<()> for T {}
            struct DebugMarker;
            impl<T: ?Sized + std::fmt::Debug> AmbiguousIfDebug<DebugMarker> for T {}
            let _ = <$ty as AmbiguousIfDebug<_>>::marker;
        };
    };
}
assert_not_debug!(DialPlan);

// ---- One connect/checkout pair; pre-content dial failure aborts (B1 A5) ----

use StepState::{Admitted, Completed, Failed, Unadmitted};

fn window(connect: StepState, checkout: StepState) -> DialWindow {
    DialWindow { connect, checkout }
}

fn check(window: DialWindow, step: DialStep, warm: bool) -> Result<(), LoaderDenial> {
    admit_connection_step(&window, step, warm)
}

fn admit(window: DialWindow, step: DialStep, warm: bool) {
    if let Err(denial) = check(window, step, warm) {
        panic!("legal connection step denied: {denial}");
    }
}

fn refuse(window: DialWindow, step: DialStep, warm: bool) {
    match check(window, step, warm) {
        Ok(()) => panic!("illegal connection step admitted"),
        Err(denial) => assert_eq!(denial, LoaderDenial::ProviderUnavailable),
    }
}

#[test]
fn fresh_window_admits_connect_first() {
    admit(window(Unadmitted, Unadmitted), DialStep::Connect, false);
}

#[test]
fn fresh_window_denies_checkout_without_warm_reuse() {
    refuse(window(Unadmitted, Unadmitted), DialStep::Checkout, false);
}

#[test]
fn warm_checkout_skips_its_unused_connect_once() {
    admit(window(Unadmitted, Unadmitted), DialStep::Checkout, true);
    // The skipped connect can never be spent afterwards.
    refuse(window(Unadmitted, Admitted), DialStep::Connect, true);
    refuse(window(Unadmitted, Admitted), DialStep::Connect, false);
}

#[test]
fn checkout_follows_completed_connect_only() {
    admit(window(Completed, Unadmitted), DialStep::Checkout, false);
    refuse(window(Admitted, Unadmitted), DialStep::Checkout, false);
    refuse(window(Unadmitted, Unadmitted), DialStep::Checkout, false);
}

#[test]
fn completed_pair_admits_no_second_pair_or_reconnect() {
    refuse(window(Completed, Completed), DialStep::Connect, false);
    refuse(window(Completed, Completed), DialStep::Checkout, false);
    refuse(window(Completed, Completed), DialStep::Connect, true);
    refuse(window(Completed, Admitted), DialStep::Connect, false);
    refuse(window(Admitted, Completed), DialStep::Connect, false);
}

#[test]
fn admitted_step_consumes_exactly_once() {
    refuse(window(Admitted, Unadmitted), DialStep::Connect, false);
    refuse(window(Completed, Admitted), DialStep::Checkout, false);
    refuse(window(Completed, Admitted), DialStep::Checkout, true);
}

#[test]
fn pre_content_dial_failure_terminally_fails_the_window() {
    // B1:211: a pre-content dial failure terminally fails the window;
    // no additional pair, reconnect or retry step is admitted after it.
    for failed in [
        window(Failed, Unadmitted),
        window(Unadmitted, Failed),
        window(Failed, Failed),
        window(Completed, Failed),
        window(Failed, Completed),
    ] {
        for warm in [false, true] {
            refuse(failed, DialStep::Connect, warm);
            refuse(failed, DialStep::Checkout, warm);
        }
    }
}
