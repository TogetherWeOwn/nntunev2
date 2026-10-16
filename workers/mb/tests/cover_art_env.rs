//! Acceptance tests: env-specific cover-art bucket mapping and interim CAA behavior.
//!
//! Authoritative bucket inventory: `docs/ops.md` ("R2 buckets", "Environments
//! matrix"). There is no bucket named `nntunev2-art`; staging uses
//! `nntunev2-art-staging` and prod uses `nntunev2-art-prod`.
//!
//! `caa_url_for_art` returns an internal `r2://` object URI. Until a public
//! R2 custom domain or `r2.dev` URL is configured, the Worker preserves stored
//! `caa_url` values unchanged. The internal object URI is not client-facing,
//! and these tests do not claim a release response uses it or that a public
//! URL is environment-correct before activation.
//!
//! No production writes, no secrets, no staging dependency: pure unit and
//! source-text assertions only.

use nntune_mb::{art_bucket_for_env, caa_url_for_art, ART_BUCKET_PROD, ART_BUCKET_STAGING};
use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> PathBuf {
    // workers/mb/tests -> workers/mb (manifest) -> repo root is two up.
    manifest_dir()
        .parent()
        .and_then(|p| p.parent())
        .expect("workers/mb sits two levels below the repo root")
        .to_path_buf()
}

#[test]
fn staging_env_resolves_staging_art_bucket() {
    assert_eq!(
        ART_BUCKET_STAGING, "nntunev2-art-staging",
        "staging bucket name must match docs/ops.md"
    );
    assert_eq!(
        art_bucket_for_env("staging"),
        "nntunev2-art-staging",
        "staging env must resolve the staging art bucket"
    );
}

#[test]
fn prod_env_resolves_prod_art_bucket() {
    assert_eq!(
        ART_BUCKET_PROD, "nntunev2-art-prod",
        "prod bucket name must match docs/ops.md"
    );
    assert_eq!(
        art_bucket_for_env("prod"),
        "nntunev2-art-prod",
        "prod env must resolve the prod art bucket"
    );
    assert_eq!(
        art_bucket_for_env("production"),
        "nntunev2-art-prod",
        "production alias must resolve the prod art bucket"
    );
}

#[test]
fn staging_and_prod_buckets_differ() {
    assert_ne!(
        ART_BUCKET_STAGING, ART_BUCKET_PROD,
        "env boundary requires distinct buckets per docs/ops.md"
    );
    assert_ne!(
        art_bucket_for_env("staging"),
        art_bucket_for_env("prod"),
        "staging and prod must not resolve to the same bucket"
    );
}

#[test]
fn caa_object_uri_uses_env_bucket_and_key() {
    let key = "art/ab/cd/0123456789abcdef-500";
    let staging_uri = caa_url_for_art(art_bucket_for_env("staging"), key);
    let prod_uri = caa_url_for_art(art_bucket_for_env("prod"), key);

    for (env, uri, bucket) in [
        ("staging", &staging_uri, "nntunev2-art-staging"),
        ("prod", &prod_uri, "nntunev2-art-prod"),
    ] {
        assert!(
            uri.starts_with("r2://"),
            "{env} CAA object URI must use the internal r2 scheme, got {uri:?}"
        );
        assert!(
            uri.contains(bucket),
            "{env} CAA object URI must identify its bucket, got {uri:?}"
        );
        assert!(
            uri.contains(key),
            "{env} CAA object URI must identify its key {key:?}, got {uri:?}"
        );
    }
}

/// Grep assertion: no bare `nntunev2-art` reference may remain in code or
/// docs. Every occurrence must be exactly the staging or prod bucket name
/// from `docs/ops.md`. Scans the four files known to carry the stale name;
/// the ops inventory itself (which documents the rename) is out of scope.
#[test]
fn no_bare_art_bucket_reference_remains() {
    let root = repo_root();
    let files = [
        "workers/mb/src/lib.rs",
        "workers/mb/README.md",
        "workers/mb/wrangler.toml.example",
        "docs/adr/0007-mb-mirror.md",
    ];
    let bare = "nntunev2-art";
    let mut offenders = Vec::new();

    for rel in files {
        let path = root.join(rel);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for (idx, line) in text.lines().enumerate() {
            let mut search_from = 0;
            while let Some(hit) = line[search_from..].find(bare) {
                let start = search_from + hit;
                let rest = &line[start + bare.len()..];
                let is_exact_allowed_bucket = ["-staging", "-prod"].iter().any(|suffix| {
                    rest.strip_prefix(suffix).is_some_and(|remaining| {
                        remaining
                            .chars()
                            .next()
                            .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-')
                    })
                });
                if !is_exact_allowed_bucket {
                    offenders.push(format!("{}:{}: {}", rel, idx + 1, line.trim()));
                    break;
                }
                search_from = start + bare.len();
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "bare `nntunev2-art` references remain (use exact -staging/-prod names per docs/ops.md):\n{}",
        offenders.join("\n")
    );
}

/// The deploy example must bind the env-specific buckets, not the bare name.
#[test]
fn wrangler_example_uses_env_specific_buckets() {
    let path = manifest_dir().join("wrangler.toml.example");
    let text = std::fs::read_to_string(&path).expect("read wrangler.toml.example");
    assert!(
        text.contains("nntunev2-art-staging"),
        "wrangler example must name the staging art bucket"
    );
    assert!(
        text.contains("nntunev2-art-prod"),
        "wrangler example must name the prod art bucket"
    );
}
