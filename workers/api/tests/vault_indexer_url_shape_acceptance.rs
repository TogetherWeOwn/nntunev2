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
//! the `:443` pin; missing an empty userinfo (`@host`, `:@host`)
//! breaks those pins; letting an obfuscated authority hide the raw `@`
//! from a literal-`://` scan (single slash, tab/newline the parser
//! strips, backslashes it treats as slashes) breaks the obfuscated
//! pins; scanning only a non-empty raw authority for `@` breaks the
//! extra-slash pins (the parser skips any number of slashes after the
//! HTTPS scheme, so the raw authority between `://` and the next `/`
//! is empty and its `@` is never seen; the no-`@` extra-slash sibling
//! pins the empty-authority refusal itself); letting a decoy `://`
//! later in the string (path or query) anchor the authority scan
//! instead of the scheme prefix breaks the decoy pins (the real
//! authority's `@` sits before the decoy, so a scan anchored there
//! never sees it); treating an empty
//! fragment as absent
//! (only rejecting non-empty fragments) breaks the bare-`#` pin;
//! ignoring the parsed fragment breaks the fragment pins. Note the
//! raw-`@` scan already rejects the plain password-only URL, so
//! `password()` is defense-in-depth rather than what makes that pin
//! pass; the obfuscated password-only pins catch a missing
//! fail-closed rule once `password()` is gone, and with the
//! empty-authority refusal in place the extra-slash password-only pin
//! is caught by that rule too. The decoy password-only pins are
//! different before the scheme-prefix anchor lands: only
//! `password()` rejects them there, so dropping it leaves them
//! validating. Once the anchor lands (reject unless the text before
//! `://` is exactly `https`), it rejects the decoy password-only
//! pins independently of `password()` — they stay rejected under a
//! `password()` mutant — leaving `password()` again pure
//! defense-in-depth.
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
        // Password-only: empty username, so a `username()`-only check passes.
        "https://:secret@index.example.com/api",
        // Empty userinfo: parsed username/password are `""`/`None`, but
        // the raw authority still carries an `@`.
        "https://@index.example.com/api",
        "https://:@index.example.com/api",
        "https://index.example.com@evil.example.com/api",
        // Obfuscated empty userinfo: the parser strips ASCII tab/newline
        // and treats backslashes as slashes for HTTPS, so these still
        // parse with a raw `@` in the authority even though no literal
        // `://` precedes it.
        "https:/@index.example.com/api",
        "https:\n//@index.example.com/api",
        "https:\t//@index.example.com/api",
        "https:/\n/@index.example.com/api",
        "https:\\\\@index.example.com/api",
        "https:\\//@index.example.com/api",
        // Obfuscated password-only: same hiding, with a `:secret` the
        // parsed-password check alone used to catch.
        "https:\n//:secret@index.example.com/api",
        "https:\\\\:secret@index.example.com/api",
        // Extra-slash empty userinfo: the parser skips any number of
        // slashes after the HTTPS scheme, so these parse with the real
        // host while the raw authority (between `://` and the next
        // `/`) is empty and its `@` is never seen by an authority scan.
        "https:///@index.example.com/api",
        "https:///:@index.example.com/api",
        "https:////@index.example.com/api",
        // No-`@` sibling: same empty raw authority, unfetchable as
        // written; it must be refused at save time, not stored.
        "https:///index.example.com/api",
        // Extra-slash password-only: the parsed-password check catches
        // this one, but under a `password()` mutant only the
        // empty-authority refusal still rejects it.
        "https:///:secret@index.example.com/api",
        // Decoy-`://` empty userinfo: no literal `://` follows the
        // scheme, so a scan anchored to the first `://` it finds later
        // in the path or query starts after the real authority and
        // never sees its `@`; parsed username/password are `""`/`None`.
        "https:@index.example.com/api://x",
        "https:/@index.example.com/api://x",
        "https:@index.example.com/api?q=://x",
        "https::@index.example.com/api://x",
        "https:@index.example.com:443/api://x",
        // Decoy password-only: the parsed-password check catches these
        // before the scheme-prefix anchor lands, but under a
        // `password()` mutant only that anchor still rejects them.
        "https::secret@index.example.com/api://x",
        "https:/:secret@index.example.com/api://x",
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
        // Bare trailing hash: `fragment()` is `Some("")`, not `None`.
        "https://index.example.com/api#",
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
