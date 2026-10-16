//! Acceptance tests: env-specific cover-art bucket and CAA URL.
//!
//! Test-first (RED): every test in this file fails on the pre-slice head
//! and passes once the Builder implements the slice. Write-only for this
//! card: the Builder implements on this branch and never edits this file.
//!
//! Authoritative bucket inventory: `docs/ops.md` ("R2 buckets", "Environments
//! matrix"). There is no bucket named `nntunev2-art`; staging uses
//! `nntunev2-art-staging` and prod uses `nntunev2-art-prod`.
//!
//! Builder contract (implement in `workers/mb/src/lib.rs`; `workers/api`
//! precedent is `crate-type = ["cdylib", "rlib"]`, add `"rlib"` here too so
//! these integration tests can link):
//!
//! ```rust
//! pub const ART_BUCKET_STAGING: &str = "nntunev2-art-staging";
//! pub const ART_BUCKET_PROD: &str = "nntunev2-art-prod";
//! pub fn art_bucket_for_env(env: &str) -> &'static str { /* ... */ }
//! pub fn caa_url_for_art(bucket: &str, key: &str) -> String { /* ... */ }
//! ```
//!
//! - `art_bucket_for_env("staging")` returns the staging bucket;
//!   `"prod"` and `"production"` return the prod bucket.
//! - `caa_url_for_art(bucket, key)` returns a URL string that embeds both
//!   the given bucket name and the given key (URL shape is the Builder's
//!   choice; the release lookup serves this value as `caa_url`).
//! - `workers/mb/wrangler.toml.example`, `workers/mb/README.md`,
//!   `workers/mb/src/lib.rs` docs and `docs/adr/0007-mb-mirror.md` must name
//!   only the env-suffixed buckets, never the bare `nntunev2-art`.
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
fn release_caa_url_points_at_env_correct_bucket() {
    let key = "art/ab/cd/0123456789abcdef-500";
    let staging_url = caa_url_for_art(art_bucket_for_env("staging"), key);
    let prod_url = caa_url_for_art(art_bucket_for_env("prod"), key);

    for (env, url) in [("staging", &staging_url), ("prod", &prod_url)] {
        assert!(
            url.contains(key),
            "{env} caa_url must embed the art key {key:?}, got {url:?}"
        );
    }
    assert!(
        staging_url.contains("nntunev2-art-staging"),
        "staging caa_url must point at the staging bucket, got {staging_url:?}"
    );
    assert!(
        !staging_url.contains("nntunev2-art-prod"),
        "staging caa_url must not point at the prod bucket, got {staging_url:?}"
    );
    assert!(
        prod_url.contains("nntunev2-art-prod"),
        "prod caa_url must point at the prod bucket, got {prod_url:?}"
    );
    assert!(
        !prod_url.contains("nntunev2-art-staging"),
        "prod caa_url must not point at the staging bucket, got {prod_url:?}"
    );
}

/// Grep assertion: no bare `nntunev2-art` reference may remain in code or
/// docs. Every occurrence must be the staging or prod suffixed name from
/// `docs/ops.md`. Scans the four files known to carry the stale name; the
/// ops inventory itself (which documents the rename) is out of scope.
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
                let is_suffixed = rest.starts_with("-staging") || rest.starts_with("-prod");
                if !is_suffixed {
                    offenders.push(format!("{}:{}: {}", rel, idx + 1, line.trim()));
                    break;
                }
                search_from = start + bare.len();
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "bare `nntunev2-art` references remain (use -staging/-prod per docs/ops.md):\n{}",
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
