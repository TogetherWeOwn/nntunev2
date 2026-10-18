//! Vault NNTP port-allowlist pins (`workers/api/src/vault.rs:55,59`).
//!
//! Contract under test:
//!
//! - [`ALLOWED_NNTP_PORTS`] is exactly `[119, 563]` (threat-model §3:
//!   "119/563 plus documented alternates only").
//! - [`nntp_port_allowed`] accepts 119/563 and rejects 80, 8080, 0 and
//!   65535.
//! - Off-list rejection is enforced where it matters: [`ProviderConfig`]
//!   save-time validation and [`authorize_pool_dial`] both map an off-list
//!   port to [`VaultError::Invalid`] without touching the network.
//!
//! Boundary vs siblings: dial-time SSRF guard behavior (host shape,
//! resolve-ALL, non-public rejection) is owned by TOG-26339; dial ceilings
//! (`MAX_DIAL_ADDRESSES`, `MAX_CONNECTIONS_CEILING`) are pinned in
//! `vault_dial_ceiling.rs`. This file pins only the allowlist literal, the
//! predicate, and the off-list rejection wiring. Tests only; no production
//! code is touched; in-test fake [`DnsResolver`], no network.
//!
//! Sensitivity (mutants this file catches): appending an alternate (e.g.
//! 443) breaks the literal pin; widening the predicate to accept 80/8080
//! breaks the rejection pins; dropping the `validate` / `authorize_pool_dial`
//! allowlist gate lets port 80 validate/dial and breaks the wiring pins.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-api --test vault_port_allowlist_acceptance
//! ```

use nntune_api::vault::{
    authorize_pool_dial, nntp_port_allowed, DnsResolver, ProviderConfig, VaultError,
    ALLOWED_NNTP_PORTS,
};
use std::net::IpAddr;

struct PublicDns;

impl DnsResolver for PublicDns {
    fn resolve(&self, _host: &str) -> Vec<IpAddr> {
        vec!["93.184.216.34".parse().unwrap()]
    }
}

fn provider_on_port(port: u16) -> ProviderConfig {
    ProviderConfig {
        host: "news.example.com".into(),
        port,
        tls: true,
        username: "u".into(),
        password: "p".into(),
        max_connections: 2,
        allow_insecure_plaintext: false,
    }
}

#[test]
fn allowlist_is_exactly_119_and_563() {
    assert_eq!(
        ALLOWED_NNTP_PORTS,
        &[119, 563],
        "NNTP allowlist must stay exactly [119, 563]; \
         a documented alternate needs a threat-model update plus this pin"
    );
}

#[test]
fn allowlisted_ports_accepted() {
    assert!(
        nntp_port_allowed(119),
        "plain NNTP port 119 must be allowed"
    );
    assert!(
        nntp_port_allowed(563),
        "implicit-TLS NNTP port 563 must be allowed"
    );
}

#[test]
fn off_list_ports_rejected() {
    // 80/8080: the HTTP ports a port-scan primitive would probe first.
    // 0/65535: the u16 range edges — neither may ever be dialed.
    for port in [80, 8080, 0, 65535] {
        assert!(
            !nntp_port_allowed(port),
            "port {port} must be rejected by the NNTP allowlist"
        );
    }
}

#[test]
fn save_time_validation_rejects_off_list_ports() {
    // On-list ports validate against a public DNS answer (save path open).
    for port in [119, 563] {
        assert!(
            provider_on_port(port).validate(&PublicDns).is_ok(),
            "allowlisted port {port} must validate"
        );
    }
    // Off-list ports fail closed as generic Invalid (no target oracle),
    // before any dial.
    for port in [80, 8080, 0, 65535] {
        assert!(
            matches!(
                provider_on_port(port).validate(&PublicDns),
                Err(VaultError::Invalid)
            ),
            "off-list port {port} must fail save-time validation"
        );
    }
}

#[test]
fn pool_dial_rejects_off_list_ports() {
    // The dial-site guard re-enforces the same invariant per connect (DNS
    // can change after the save; the save-time check alone is bypassable).
    for port in [80, 8080, 0, 65535] {
        assert!(
            matches!(
                authorize_pool_dial("news.example.com", port, &PublicDns),
                Err(VaultError::Invalid)
            ),
            "off-list port {port} must fail the pool dial guard"
        );
    }
    assert!(
        authorize_pool_dial("news.example.com", 119, &PublicDns).is_ok(),
        "allowlisted port 119 must pass the pool dial guard"
    );
}
