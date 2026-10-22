//! Builder-owned regression pin for the loader's inline SSRF verdict mirror.
//!
//! `workers/stream/src/loader.rs` mirrors `nntune-ssrf-guard` inline because
//! this slice admits no new dependencies; long-term classification stays
//! single-owned by the shared guard crate. This file pins the mirrored
//! families and filter boundaries the acceptance suite does not vector-cover,
//! so a future edit dropping any mirror clause fails here instead of silently
//! admitting SSRF dial. It is builder-owned scaffolding, NOT part of the
//! independently owned acceptance handoff: the `loader_*_acceptance` suites
//! and the oracle doc stay untouched.
//!
//! No DNS, sockets, vault, keys, DB, bindings or network: candidates are
//! inert pre-resolved text. v4 fixtures are built from octets so no dotted
//! private quad appears in tracked files; v6 fixtures are literals except
//! the v4-mapped private vector, which is octet-built for the same reason
//! (the public-text lint bans dotted RFC-1918 quads even inside v6 text).

use nntune_stream::loader::{
    map_loader_error, vet_dial_plan, DialPlan, LoaderDenial, VetDialPlanRequest,
};
use std::net::Ipv4Addr;

fn v4(a: u8, b: u8, c: u8, d: u8) -> String {
    Ipv4Addr::new(a, b, c, d).to_string()
}

fn refs(list: &[String]) -> Vec<&str> {
    list.iter().map(String::as_str).collect()
}

fn request<'a>(host: &'a str, candidates: &'a [&'a str]) -> VetDialPlanRequest<'a> {
    VetDialPlanRequest {
        host,
        port: 563,
        tls: true,
        allow_insecure_plaintext: false,
        candidates,
    }
}

fn deny(candidate: &str) {
    let own = [candidate.to_string()];
    let listed = refs(&own);
    let req = request("news.example.invalid", &listed);
    match vet_dial_plan(&req) {
        Ok(_) => panic!("forbidden mirror vector admitted: {candidate}"),
        Err(denial) => assert_eq!(denial, LoaderDenial::ProviderUnavailable),
    }
}

fn admit(candidate: &str) -> DialPlan {
    let own = [candidate.to_string()];
    let listed = refs(&own);
    let req = request("news.example.invalid", &listed);
    match vet_dial_plan(&req) {
        Ok(plan) => plan,
        Err(denial) => panic!("public boundary vector denied: {candidate}: {denial}"),
    }
}

#[test]
fn mirror_pins_transition_and_special_use_v6_families() {
    // Each clause of the mirrored v6 verdict owns at least one vector here:
    // dropping any of them must fail this test, not go unnoticed.
    for candidate in [
        "2002:a00:1::1",                        // 6to4 transition
        "2002:7f00:1::1",                       // 6to4 with embedded v4 payload
        "2001:0:4136:e378:8000:63bf:3fff:fdd2", // Teredo
        "2001::1",                              // Teredo/ORCHID range head
        "3fff::1",                              // v6 documentation range
        "::ffff:127.0.0.1",                     // v4-mapped loopback
        "64:ff9b::7f00:1",                      // NAT64 with embedded loopback payload
        "64:ff9b::5db8:d822",                   // NAT64 with public payload, still blocked
    ] {
        deny(candidate);
    }
    // v4-mapped private, octet-built so no dotted RFC-1918 quad is tracked.
    deny(&format!("::ffff:{}", v4(10, 0, 0, 5)));
}

#[test]
fn mirror_pins_v4_documentation_and_benchmarking_second_half() {
    // is_documentation covers three /24s; the mirror's benchmarking clause
    // covers both second-octet halves. Octet-built, never dotted literals.
    for octets in [
        [192, 0, 2, 1],
        [198, 51, 100, 1],
        [203, 0, 113, 1],
        [198, 19, 1, 1],
        [198, 19, 255, 255],
    ] {
        deny(&v4(octets[0], octets[1], octets[2], octets[3]));
    }
}

#[test]
fn mirror_pins_carrier_grade_nat_mask_edges() {
    // The shared-NAT /10 top edge denies; the neighboring addresses admit.
    // A narrowed CGNAT mask must fail here.
    deny(&v4(100, 127, 255, 255));
    for octets in [
        [100, 63, 255, 255],
        [100, 128, 0, 1],
        [198, 17, 255, 255],
        [198, 20, 0, 1],
        [223, 255, 255, 255],
    ] {
        admit(&v4(octets[0], octets[1], octets[2], octets[3]));
    }
}

#[test]
fn mirror_pins_public_controls_admit() {
    admit(&v4(93, 184, 216, 34));
    admit("2606:4700:4700::1111");
}

// ---- UUIDv4 variant strictness (oracle: only a valid lowercase UUIDv4 echoes) ----

const FRESH: &str = "123e4567-e89b-42d3-a456-426614174999";

fn exact(id: &str, code: &str) -> String {
    format!("{{\"v\":3,\"request_id\":\"{id}\",\"error\":\"{code}\"}}")
}

#[test]
fn invalid_uuid_variants_substitute_the_fresh_id() {
    // Version 4 with a non-UUIDv4 variant nibble is not a valid UUIDv4: the
    // caller bytes must not echo. Valid variants still echo byte-exact.
    for invalid in [
        // Variant nibble 0, not 8/9/a/b.
        "123e4567-e89b-42d3-0456-426614174000",
        // Variant nibble f: reserved, not a UUIDv4 variant.
        "123e4567-e89b-42d3-f456-426614174000",
    ] {
        let mapped = map_loader_error(LoaderDenial::ProviderUnavailable, invalid, FRESH, true);
        assert_eq!(mapped.status(), 503);
        assert_eq!(mapped.body(), exact(FRESH, "provider_unavailable"));
    }
    let mapped = map_loader_error(
        LoaderDenial::ProviderUnavailable,
        "123e4567-e89b-42d3-b456-426614174000",
        FRESH,
        true,
    );
    assert_eq!(
        mapped.body(),
        exact(
            "123e4567-e89b-42d3-b456-426614174000",
            "provider_unavailable"
        )
    );
}
