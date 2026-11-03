//! CC0 fixture-audio access for Rust tests.
//!
//! All dev/test audio is CC0 1.0 (or a reviewed equal), recorded in a
//! committed manifest under `fixtures/cc0/` — see
//! `docs/fixture-audio-licences.md`. The audio bytes live in the R2
//! staging bucket, not in git. This crate is the Rust side of the
//! fetch-by-key helper: it drives `fixtures/cc0/fetch_fixture.py`
//! (stdlib-only Python; the TS helper in `e2e/helpers/` drives the
//! same script, so both languages share one implementation, one cache
//! and one verification path).
//!
//! Skip semantics: when R2 read access is not configured in the
//! environment (the normal case in CI today), the fetcher exits 77 and
//! this crate returns [`Fixture::Skipped`] with the reason. Tests must
//! treat that as "skip this case", never as a failure:
//!
//! ```no_run
//! use nntune_test_fixtures::{cc0_track, Fixture};
//!
//! let path = match cc0_track(1).expect("fixture fetch failed") {
//!     Fixture::Ready(path) => path,
//!     Fixture::Skipped(reason) => {
//!         eprintln!("skipping: {reason}");
//!         return;
//!     }
//! };
//! ```

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Exit code the Python fetcher uses for "skip: not configured here".
const FETCHER_SKIP_EXIT: i32 = 77;
const FETCHER_RELATIVE_PATH: &str = "fixtures/cc0/fetch_fixture.py";
const REPO_ROOT_ENV: &str = "NNTUNE_FIXTURE_REPO_ROOT";

/// Environment variables that configure the fetcher's R2 transport.
/// Tests clear these for the child process to observe the skip path
/// deterministically; nothing else should need the list.
const FETCHER_CONFIG_ENV: [&str; 7] = [
    "NNTUNE_FIXTURE_BASE_URL",
    "NNTUNE_FIXTURE_R2_ACCESS_KEY_ID",
    "NNTUNE_FIXTURE_R2_SECRET_ACCESS_KEY",
    "NNTUNE_FIXTURE_R2_ENDPOINT",
    "NNTUNE_FIXTURE_R2_ACCOUNT_ID",
    "NNTUNE_FIXTURE_R2_BUCKET",
    "NNTUNE_FIXTURE_CACHE_DIR",
];

/// Outcome of a fixture request: verified bytes on disk, or a skip.
#[derive(Debug)]
pub enum Fixture {
    /// Absolute path of the verified cached file (bytes + sha256
    /// already checked against the manifest by the fetcher).
    Ready(PathBuf),
    /// R2 read access is not configured (or `python3` is missing); the
    /// string is the fetcher's human-readable reason. Skip, don't fail.
    Skipped(String),
}

/// Fetch track `position` from the default CC0 fixture album
/// (HoliznaCC0, *Busted Guitar (JAZZ)*; see the committed manifest).
///
/// # Errors
///
/// Returns the fetcher's stderr text when it fails for a real reason
/// (bad position, network error, manifest mismatch). "Not configured"
/// is **not** an error: it comes back as [`Fixture::Skipped`].
pub fn cc0_track(position: u32) -> Result<Fixture, String> {
    run_fetcher(&["--position", &position.to_string()], false)
}

/// Fetch one file from a specific manifest, addressed by the track
/// `file` value exactly as the manifest lists it (e.g.
/// `"flac/01 - 1 (jazz).flac"`). `manifest` is repo-relative, e.g.
/// `"fixtures/cc0/<album>/manifest.json"`. The fetcher refuses files
/// the manifest does not list.
///
/// # Errors
///
/// Same contract as [`cc0_track`].
pub fn cc0_file(manifest: &str, file: &str) -> Result<Fixture, String> {
    run_fetcher(&["--manifest", manifest, file], false)
}

fn repo_root() -> Result<PathBuf, String> {
    if let Ok(root) = std::env::var(REPO_ROOT_ENV) {
        let root = PathBuf::from(root);
        if root.join(FETCHER_RELATIVE_PATH).is_file() {
            return Ok(root);
        }
        return Err(format!(
            "{REPO_ROOT_ENV}={} does not contain {FETCHER_RELATIVE_PATH}",
            root.display()
        ));
    }
    // This crate lives at crates/nntune-test-fixtures, so the repo
    // root is two directories up from its manifest dir. Walk up anyway
    // so a future move does not silently break the lookup.
    let mut dir = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    loop {
        if dir.join(FETCHER_RELATIVE_PATH).is_file() {
            return Ok(dir);
        }
        if !dir.pop() {
            return Err(format!(
                "could not find {FETCHER_RELATIVE_PATH} above {}",
                env!("CARGO_MANIFEST_DIR")
            ));
        }
    }
}

fn run_fetcher(args: &[&str], clear_config_env: bool) -> Result<Fixture, String> {
    let root = repo_root()?;
    let mut command = Command::new("python3");
    command
        .arg(root.join(FETCHER_RELATIVE_PATH))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if clear_config_env {
        for var in FETCHER_CONFIG_ENV {
            command.env_remove(var);
        }
        // Also point the cache at an empty dir: a verified cache hit
        // needs no transport and would make the skip test flaky on a
        // machine where a real fetch already happened.
        command.env(
            "NNTUNE_FIXTURE_CACHE_DIR",
            std::env::temp_dir().join(format!("nntune-test-fixtures-{}", std::process::id())),
        );
    }
    let output = match command.output() {
        Ok(output) => output,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(Fixture::Skipped(
                "CC0 fixture fetch skipped: python3 not found on PATH".to_string(),
            ));
        }
        Err(err) => return Err(format!("spawning the fixture fetcher failed: {err}")),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    match output.status.code() {
        Some(0) => {
            if stdout.is_empty() {
                Err("fixture fetcher exited 0 without printing a path".to_string())
            } else {
                Ok(Fixture::Ready(PathBuf::from(stdout)))
            }
        }
        Some(code) if code == FETCHER_SKIP_EXIT => Ok(Fixture::Skipped(stderr)),
        _ => Err(if stderr.is_empty() {
            format!("fixture fetcher failed ({})", output.status)
        } else {
            stderr
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_root_finds_the_fetcher() {
        let root = repo_root().expect("repo root");
        assert!(root.join(FETCHER_RELATIVE_PATH).is_file());
    }

    #[test]
    fn unconfigured_environment_skips_with_a_reason() {
        // Clear the fetcher's config for the child process only, so the
        // skip path is what every environment without R2 creds sees.
        match run_fetcher(&["--position", "1"], true) {
            Ok(Fixture::Skipped(reason)) => {
                assert!(reason.contains("skipped"), "reason: {reason}");
            }
            Ok(Fixture::Ready(path)) => {
                panic!("unexpected Ready without config: {}", path.display())
            }
            Err(err) => panic!("skip must not be an error: {err}"),
        }
    }

    #[test]
    fn unknown_position_is_an_error_not_a_skip() {
        // Position 99 is rejected from the manifest before any
        // transport is needed, so this is deterministic offline.
        let err = run_fetcher(&["--position", "99"], true).expect_err("unknown position must fail");
        assert!(err.contains("position 99"), "error: {err}");
    }
}
