//! Manifest guard + cache-policy acceptance tests (TOG-25320).
//!
//! Paired acceptance-test child for the build card TOG-25322
//! (`cache: R2 eviction and TTL policy with archive reservation guard`).
//! Test files only: the builder builds on this branch and never edits them.
//!
//! Policy under test (see `docs/ops.md` bucket inventory):
//! - Audio/art buckets (`nntunev2-audio-*`, `nntunev2-art-*`) are the only R2
//!   buckets a Worker may bind.
//! - Archive buckets (`nntunev2-archive-staging`, `nntunev2-archive-prod`)
//!   are reserved capacity with NO Worker consumer; any `[[r2_buckets]]`
//!   binding that targets one must fail the guard.
//!
//! Expected state before the build (TDD red):
//! - `archive_bound_fixtures_are_rejected` passes: the RED fixtures ARE
//!   rejected by the guard (fixture red, guard working).
//! - `current_worker_manifests_have_no_archive_binding` passes: today's
//!   manifests bind no archive bucket (current green).
//! - `cache_policy_doc_pins_ttl_eviction_and_archive_reservation` FAILS:
//!   `docs/cache-policy.md` does not exist yet. The builder (TOG-25322) adds
//!   the policy table + CI assertion and turns this green without touching
//!   these files.
//!
//! No Cloudflare API calls: everything here parses local manifest text.

use std::path::{Path, PathBuf};

/// Reserved archive buckets from `docs/ops.md`. No Worker may bind these.
const ARCHIVE_BUCKETS: [&str; 2] = ["nntunev2-archive-staging", "nntunev2-archive-prod"];

/// Buckets the policy table (TOG-25322) must cover per environment.
const POLICY_BUCKETS: [&str; 4] = [
    "nntunev2-audio-staging",
    "nntunev2-audio-prod",
    "nntunev2-art-staging",
    "nntunev2-art-prod",
];

/// Worker manifests the guard scans, relative to the repository root.
const WORKER_MANIFESTS: [&str; 4] = [
    "workers/stream/wrangler.toml",
    "workers/ingest/wrangler.toml.example",
    "workers/mb/wrangler.toml.example",
    "workers/api/wrangler.toml.example",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// A binding is forbidden when it names a reserved archive bucket.
/// Substring matching keeps the guard robust against future per-env renames
/// (`...-archive-...` stays reserved) while the exact names pin today's
/// inventory from `docs/ops.md`.
fn is_archive_bucket(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    lower.contains("archive") || ARCHIVE_BUCKETS.contains(&lower.as_str())
}

/// `bucket_name` values inside `[[r2_buckets]]` sections that violate the
/// archive reservation. Empty = manifest is clean.
fn archive_bindings(manifest: &str) -> Vec<String> {
    let mut offenders = Vec::new();
    let mut in_r2_section = false;
    for raw_line in manifest.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_r2_section = line.starts_with("[[")
                && line
                    .trim_start_matches('[')
                    .trim_start()
                    .starts_with("r2_buckets");
            continue;
        }
        if !in_r2_section {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "bucket_name" {
            continue;
        }
        // Strip an inline `#` comment, then surrounding quotes/whitespace.
        let value = value.split('#').next().unwrap_or("").trim();
        let name = value.trim_matches(|c| c == '"' || c == '\'').trim();
        if !name.is_empty() && is_archive_bucket(name) {
            offenders.push(name.to_string());
        }
    }
    offenders
}

#[test]
fn archive_bound_fixtures_are_rejected() {
    // RED fixtures: each must trip the guard. If this test fails, the guard
    // helper above no longer detects an archive binding — fix the helper,
    // never the fixtures.
    for fixture in [
        include_str!("fixtures/r2_guard_archive_staging.toml"),
        include_str!("fixtures/r2_guard_archive_prod.toml"),
    ] {
        let offenders = archive_bindings(fixture);
        assert!(
            !offenders.is_empty(),
            "guard must reject an archive-bound manifest, got none"
        );
        assert!(
            offenders.iter().any(|b| b.contains("archive")),
            "rejection must name the archive bucket, got {offenders:?}"
        );
    }
}

#[test]
fn current_worker_manifests_have_no_archive_binding() {
    // GREEN side: today's manifests bind no archive bucket. This scans the
    // real files (not copies) so a future archive binding turns it red.
    let root = repo_root();
    for rel in WORKER_MANIFESTS {
        let path = root.join(rel);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("worker manifest must exist: {rel}"));
        assert!(
            archive_bindings(&text).is_empty(),
            "{rel} must not bind an archive bucket"
        );
    }
}

#[test]
fn cache_policy_doc_pins_ttl_eviction_and_archive_reservation() {
    // TDD RED before the TOG-25322 build: `docs/cache-policy.md` does not
    // exist yet. The builder adds the TTL/eviction table there; this test
    // then passes unmodified.
    let path = repo_root().join("docs/cache-policy.md");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!("docs/cache-policy.md must exist (TOG-25322): TTL/eviction table per bucket")
    });
    for bucket in POLICY_BUCKETS {
        assert!(
            text.contains(bucket),
            "policy doc must table bucket {bucket}"
        );
    }
    for bucket in ARCHIVE_BUCKETS {
        assert!(
            text.contains(bucket),
            "policy doc must name reserved bucket {bucket}"
        );
    }
    let lower = text.to_lowercase();
    assert!(
        lower.contains("no worker consumer"),
        "policy doc must state archive buckets have no Worker consumer"
    );
    assert!(text.contains("TTL"), "policy doc must pin TTL per bucket");
    assert!(
        lower.contains("evict"),
        "policy doc must pin eviction per bucket"
    );
}
