//! Indexer URL shape pins at the vault boundary (`IndexerConfig::validate`).
//!
//! Contract under test (acceptance spec only; no implementation is
//! consulted by this file's author):
//!
//! - An explicit non-443 port yields [`VaultError::Invalid`].
//! - Embedded userinfo yields [`VaultError::Invalid`].
//! - A fragment yields [`VaultError::Invalid`].
//! - An explicit `:443`, a default-port URL, and otherwise clean HTTPS
//!   URLs still validate.
//!
//! Every rejected URL below is otherwise valid — HTTPS scheme, public
//! host (via the in-test fake [`DnsResolver`]), non-empty API key — so a
//! failure can only be caused by the URL-shape rule under test, never by
//! the pre-existing SSRF/scheme guards. Tests only; no production code
//! is touched by this card, and the build card must not edit this file.
//!
//! Sensitivity (mutants this file catches): accepting any explicit port
//! breaks the non-443 pins; rejecting an explicit `:443` (e.g. by
//! string-matching a `:NNNN` suffix instead of the parsed port) breaks
//! the `:443` pin; checking only `username()` and missing a password-only
//! authority is not pinned — user and user:password forms are; ignoring
//! the parsed fragment breaks the fragment pins.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-api --test vault_indexer_url_shape_acceptance
//! ```

use nntune_api::vault::{DnsResolver, IndexerConfig, VaultError};
use std::net::IpAddr;

/// Every host resolves to one public address, so DNS/SSRF checks pass
/// and only the URL-shape rule can reject.
struct PublicDns;

impl DnsResolver for PublicDns {
    fn resolve(&self, _host: &str) -> Vec<IpAddr> {
        vec!["93.184.216.34".parse().unwrap()]
    }
}

fn indexer(url: &str) -> IndexerConfig {
    IndexerConfig {
        url: url.into(),
        api_key: "idx-key".into(),
    }
}

#[test]
fn explicit_non_443_port_is_invalid() {
    // 8443/8080: the alternate-service ports an SSRF probe reaches for.
    // 80: plaintext HTTP port behind an HTTPS scheme. 0: the range edge.
    for url in [
        "https://index.example.com:8443/api",
        "https://index.example.com:8080/api",
        "https://index.example.com:80/api",
        "https://index.example.com:0/api",
    ] {
        assert!(
            matches!(indexer(url).validate(&PublicDns), Err(VaultError::Invalid)),
            "explicit non-443 port must be Invalid: {url}"
        );
    }
}

#[test]
fn embedded_userinfo_is_invalid() {
    // Userinfo has no place in a stored indexer URL: the API key travels
    // separately, and `trusted@attacker` userinfo disguises the real host.
    for url in [
        "https://user@index.example.com/api",
        "https://user:secret@index.example.com/api",
        "https://index.example.com@evil.example.com/api",
    ] {
        assert!(
            matches!(indexer(url).validate(&PublicDns), Err(VaultError::Invalid)),
            "embedded userinfo must be Invalid: {url}"
        );
    }
}

#[test]
fn fragment_is_invalid() {
    // A fragment is never sent to a server; in a stored base URL it can
    // only be smuggled data or a copy-paste error. Refuse it at save time.
    for url in [
        "https://index.example.com/api#section",
        "https://index.example.com/api#t=1",
        "https://index.example.com/#frag",
    ] {
        assert!(
            matches!(indexer(url).validate(&PublicDns), Err(VaultError::Invalid)),
            "fragment must be Invalid: {url}"
        );
    }
}

#[test]
fn explicit_443_still_validates() {
    assert!(
        indexer("https://index.example.com:443/api")
            .validate(&PublicDns)
            .is_ok(),
        "explicit :443 is the HTTPS default port and must validate"
    );
}

#[test]
fn default_port_and_clean_urls_still_validate() {
    for url in [
        "https://index.example.com/api",
        "https://index.example.com/",
        "https://index.example.com",
        "https://index.example.com/api/v2/search",
    ] {
        assert!(
            indexer(url).validate(&PublicDns).is_ok(),
            "clean default-port URL must validate: {url}"
        );
    }
}
