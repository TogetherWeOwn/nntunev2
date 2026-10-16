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
        Some("nntunev2-art-staging"),
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
        Some("nntunev2-art-prod"),
        "prod env must resolve the prod art bucket"
    );
    assert_eq!(
        art_bucket_for_env("production"),
        Some("nntunev2-art-prod"),
        "production alias must resolve the prod art bucket"
    );
}

#[test]
fn unknown_and_empty_env_resolves_to_none() {
    for env in ["", "dev"] {
        assert_eq!(
            art_bucket_for_env(env),
            None,
            "unknown/empty env {env:?} must resolve to None, never staging or prod art"
        );
        assert_ne!(
            art_bucket_for_env(env),
            Some(ART_BUCKET_STAGING),
            "unknown/empty env {env:?} must never resolve the staging art bucket"
        );
        assert_ne!(
            art_bucket_for_env(env),
            Some(ART_BUCKET_PROD),
            "unknown/empty env {env:?} must never resolve the prod art bucket"
        );
    }
}

#[test]
fn staging_and_prod_buckets_differ() {
    assert_ne!(
        ART_BUCKET_STAGING, ART_BUCKET_PROD,
        "env boundary requires distinct buckets per docs/ops.md"
    );
    assert_eq!(
        art_bucket_for_env("staging"),
        Some(ART_BUCKET_STAGING),
        "staging env must resolve Some(staging bucket)"
    );
    assert_eq!(
        art_bucket_for_env("prod"),
        Some(ART_BUCKET_PROD),
        "prod env must resolve Some(prod bucket)"
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
    let staging_bucket = art_bucket_for_env("staging").expect("staging env must resolve");
    let prod_bucket = art_bucket_for_env("prod").expect("prod env must resolve");
    let staging_uri = caa_url_for_art(staging_bucket, key);
    let prod_uri = caa_url_for_art(prod_bucket, key);

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
/// Parses per-env sections and per-block bindings so a swapped `bucket_name`
/// (M2), a deleted `ART_CACHE` block, a swapped `MB_DB`/`MB_CACHE` binding,
/// or a deleted per-env `MB_DB` block fails instead of passing silently.
///
/// The example's env sections are commented lines (`# [[env.staging...`),
/// so headers are matched without the leading `# ` (sections are sliced at
/// the first `[[`, which drops the `# ` prefix on the opening header).
fn block_for<'a>(section: &'a str, header: &str) -> &'a str {
    let start = section
        .find(header)
        .unwrap_or_else(|| panic!("missing block {header}"));
    let after = start + header.len();
    let rest = &section[after..];
    let mut end = rest.len();
    for marker in ["\n# [[", "\n# ["] {
        if let Some(idx) = rest.find(marker) {
            end = end.min(idx);
        }
    }
    &section[start..after + end]
}

#[test]
fn wrangler_example_uses_env_specific_buckets() {
    let path = manifest_dir().join("wrangler.toml.example");
    let text = std::fs::read_to_string(&path).expect("read wrangler.toml.example");
    let staging_start = text
        .find("[[env.staging")
        .expect("wrangler example must define a staging env section");
    let production_start = text
        .find("[[env.production")
        .expect("wrangler example must define a production env section");
    assert!(
        staging_start < production_start,
        "staging env section must precede production env section"
    );
    let staging_section = &text[staging_start..production_start];
    let production_section = &text[production_start..];

    for (env, section, short, bucket, other_bucket) in [
        (
            "staging",
            staging_section,
            "staging",
            "nntunev2-art-staging",
            "nntunev2-art-prod",
        ),
        (
            "production",
            production_section,
            "production",
            "nntunev2-art-prod",
            "nntunev2-art-staging",
        ),
    ] {
        let hyperdrive_header = format!("[[env.{short}.hyperdrive]]");
        let kv_header = format!("[[env.{short}.kv_namespaces]]");
        let r2_header = format!("[[env.{short}.r2_buckets]]");
        assert!(
            section.contains(&hyperdrive_header),
            "{env} section must declare its hyperdrive block"
        );
        assert!(
            section.contains(&kv_header),
            "{env} section must declare its kv block"
        );
        assert!(
            section.contains(&r2_header),
            "{env} section must declare its r2_buckets block"
        );
        let hyperdrive = block_for(section, &hyperdrive_header);
        let kv = block_for(section, &kv_header);
        let r2 = block_for(section, &r2_header);

        assert!(
            hyperdrive.contains("binding = \"MB_DB\""),
            "{env} hyperdrive block must bind MB_DB (deleted per-env MB_DB must fail)"
        );
        assert!(
            !hyperdrive.contains("binding = \"MB_CACHE\""),
            "{env} hyperdrive block must not bind MB_CACHE (swapped MB_DB/MB_CACHE must fail)"
        );
        assert!(
            kv.contains("binding = \"MB_CACHE\""),
            "{env} kv block must bind MB_CACHE"
        );
        assert!(
            !kv.contains("binding = \"MB_DB\""),
            "{env} kv block must not bind MB_DB (swapped MB_DB/MB_CACHE must fail)"
        );
        assert!(
            r2.contains("binding = \"ART_CACHE\""),
            "{env} r2 block must bind ART_CACHE (deleted ART_CACHE must fail)"
        );
        assert!(
            r2.contains(&format!("bucket_name = \"{bucket}\"")),
            "{env} r2 block must bind bucket {bucket} (swapped buckets must fail)"
        );
        assert!(
            !r2.contains(other_bucket),
            "{env} r2 block must not reference the other env bucket {other_bucket} (swap must fail)"
        );
    }
}

#[test]
fn ops_inventory_lists_exact_art_buckets() {
    let path = repo_root().join("docs/ops.md");
    let text = std::fs::read_to_string(&path).expect("read docs/ops.md authoritative inventory");
    for (bucket, env) in [(ART_BUCKET_STAGING, "staging"), (ART_BUCKET_PROD, "prod")] {
        assert!(
            text.contains(&format!("| `{bucket}` | {env} |")),
            "docs/ops.md R2 buckets table must list {bucket} for {env} (rename in ops only must fail)"
        );
    }
    assert!(
        text.contains(&format!(
            "| R2 artwork / MB Worker | `{}` | `{}` |",
            ART_BUCKET_STAGING, ART_BUCKET_PROD
        )),
        "docs/ops.md Environments matrix must pin both art buckets"
    );
    let art_cache_line = text
        .lines()
        .find(|line| line.contains("`ART_CACHE`"))
        .expect("docs/ops.md Worker bindings must document ART_CACHE");
    assert!(
        art_cache_line.contains(ART_BUCKET_STAGING) && art_cache_line.contains(ART_BUCKET_PROD),
        "docs/ops.md ART_CACHE binding row must pin both buckets, got {art_cache_line:?}"
    );
}
