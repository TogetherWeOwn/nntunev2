//! Version-pinned KEK selection survives retirement of the user credential API.
//! No routes, database, network or credential payloads are involved.

use nntune_api::{handlers::Config, vault};

fn config(key: [u8; 32], version: u32) -> Config {
    Config::new(
        "example.com",
        "https://example.com",
        vec![7; 32],
        key,
        version,
        3600,
        false,
        false,
        None,
    )
    .unwrap()
}

fn random_key() -> [u8; 32] {
    nntune_api::random::secure_random_bytes().unwrap()
}

#[test]
fn lookup_is_version_pinned_and_unknown_versions_never_fall_back() {
    let (older, prior, current) = (random_key(), random_key(), random_key());
    let config = config(current, 11).with_prior_keks(vec![(3, prior), (1, older)]);
    for (version, key) in [(1, &older), (3, &prior), (11, &current)] {
        let blob = vault::encrypt_vault(key, version, "row-owner", b"opaque payload").unwrap();
        let selected = config.kek_by_version(version).expect("provisioned version");
        assert!(vault::decrypt_vault(selected, "row-owner", &blob)
            .is_ok_and(|plain| plain == b"opaque payload"));
    }
    // Even identical key bytes do not authorize an unprovisioned version.
    let unknown = vault::encrypt_vault(&current, 97, "row-owner", b"opaque payload").unwrap();
    assert!(vault::decrypt_vault(&current, "row-owner", &unknown).is_ok());
    assert!(config.kek_by_version(unknown.kek_version).is_none());
}

#[test]
fn current_key_wins_over_a_same_version_retained_entry() {
    let (current, decoy) = (random_key(), random_key());
    let config = config(current, 11).with_prior_keks(vec![(11, decoy)]);
    let selected = config.kek_by_version(11).expect("current version");
    let blob = vault::encrypt_vault(selected, 11, "row-owner", b"opaque payload").unwrap();
    assert!(vault::decrypt_vault(&current, "row-owner", &blob).is_ok());
    assert!(vault::decrypt_vault(&decoy, "row-owner", &blob).is_err());
}

#[test]
fn legacy_single_key_lookup_stays_version_pinned() {
    let key = random_key();
    let config = config(key, 41);
    assert!(config.prior_keks.is_empty());
    let selected = config.kek_by_version(41).expect("legacy key");
    let blob = vault::encrypt_vault(selected, 41, "row-owner", b"opaque payload").unwrap();
    assert!(vault::decrypt_vault(&key, "row-owner", &blob).is_ok());
    assert!(config.kek_by_version(97).is_none());
}
