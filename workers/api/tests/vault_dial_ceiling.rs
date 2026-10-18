//! Vault dial-address truncation + max-connections ceiling pins
//! (`workers/api/src/vault.rs::resolve_public_target`,
//! `ProviderConfig::validate`).
//!
//! Contract under test:
//!
//! - [`MAX_DIAL_ADDRESSES`] is 4 and [`MAX_CONNECTIONS_CEILING`] is 50.
//! - `ProviderConfig::validate` resolves ALL DNS answers, rejects the whole
//!   target when ANY answer is forbidden, and only then retains the first 4
//!   addresses in order for a pinned dial.
//! - `max_connections` 0 and 51 map to [`VaultError::Invalid`]; 50 passes.
//!
//! Boundary vs the pool dial-time guard: this file pins the vault
//! validate-then-truncate ordering plus the ceiling. The pool dial path,
//! DNS impl, and any `src` change are out of scope — tests only, in-test
//! fake [`DnsResolver`], no network.
//!
//! Sensitivity (mutants this file catches): truncating before the forbidden
//! check lets a forbidden 5th/6th answer pass; `MAX_DIAL_ADDRESSES` 5 keeps
//! 5 addresses and breaks the length/order pin; a ceiling of 51 accepts
//! `max_connections` 51; a ceiling of 49 (or `>=` instead of `>`) rejects
//! the legal 50; dropping the `== 0` arm accepts 0.
//!
//! Distinct from the in-module
//! `validated_target_retains_only_a_bounded_address_prefix_after_checking_all`
//! unit test (5 answers): this file pins 6 answers through the public
//! `ProviderConfig::validate` surface plus the ceiling boundary.

use nntune_api::vault::{
    DnsResolver, ProviderConfig, VaultError, MAX_CONNECTIONS_CEILING, MAX_DIAL_ADDRESSES,
};
use std::net::IpAddr;

struct ListDns(Vec<IpAddr>);

impl DnsResolver for ListDns {
    fn resolve(&self, _host: &str) -> Vec<IpAddr> {
        self.0.clone()
    }
}

fn public_six() -> Vec<IpAddr> {
    [
        "8.8.8.8",
        "1.1.1.1",
        "9.9.9.9",
        "4.2.2.1",
        "208.67.222.222",
        "93.184.216.34",
    ]
    .iter()
    .map(|s| s.parse().unwrap())
    .collect()
}

fn provider_with_max(max_connections: u32) -> ProviderConfig {
    ProviderConfig {
        host: "news.example.com".into(),
        port: 119,
        tls: true,
        username: "u".into(),
        password: "p".into(),
        max_connections,
        allow_insecure_plaintext: false,
    }
}

#[test]
fn dial_and_connection_ceiling_values_are_four_and_fifty() {
    assert_eq!(MAX_DIAL_ADDRESSES, 4);
    assert_eq!(MAX_CONNECTIONS_CEILING, 50);
}

#[test]
fn six_public_addresses_succeed_with_first_four_in_order() {
    let answers = public_six();
    let target = provider_with_max(4)
        .validate(&ListDns(answers.clone()))
        .expect("6 public answers must validate");
    assert_eq!(target.addresses(), &answers[..4]);
    assert_eq!(target.host(), "news.example.com");
    assert_eq!(target.port(), 119);
}

#[test]
fn max_connections_zero_and_fifty_one_rejected_fifty_passes() {
    let dns = ListDns(vec!["93.184.216.34".parse().unwrap()]);
    // `ValidatedTarget` carries no `PartialEq`, so pin the error variant
    // with `matches!` instead of comparing the whole `Result`.
    assert!(
        matches!(
            provider_with_max(0).validate(&dns),
            Err(VaultError::Invalid)
        ),
        "max_connections 0 must be Invalid"
    );
    assert!(
        matches!(
            provider_with_max(51).validate(&dns),
            Err(VaultError::Invalid)
        ),
        "max_connections 51 must be Invalid"
    );
    assert!(
        provider_with_max(50).validate(&dns).is_ok(),
        "max_connections 50 (the ceiling) must pass"
    );
}

#[test]
fn forbidden_at_any_position_including_fifth_rejects_whole_target() {
    // TEST-NET-1 documentation address: forbidden by the SSRF guard
    // (`is_documentation`) yet lint-clean for this public repo.
    let forbidden: IpAddr = "192.0.2.1".parse().unwrap();
    // Every slot — including the 5th (index 4) and 6th (index 5) beyond the
    // retained prefix — must reject the whole target. A truncate-before-
    // validate implementation would wrongly accept the beyond-prefix cases.
    for pos in 0..6 {
        let mut answers = public_six();
        answers[pos] = forbidden;
        assert!(
            matches!(
                provider_with_max(4).validate(&ListDns(answers)),
                Err(VaultError::Invalid)
            ),
            "forbidden IP at position {pos} must reject the whole target"
        );
    }
}
