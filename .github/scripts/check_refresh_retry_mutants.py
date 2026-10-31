#!/usr/bin/env python3
"""Require real assertion failures for the refresh boundary regressions.

Uses only the existing loopback disposable-PostgreSQL fixture. Source changes
are temporary and always restored; compiler errors never count as killed mutants.
Local compiling invocations retain the repository's bounded Cargo pool.
"""
import os
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[2]
WORKER = ROOT / "workers/api/src/worker_entry.rs"
HANDLERS = ROOT / "workers/api/src/handlers.rs"
CASES = (
    (
        "retry digest forwarding",
        WORKER,
        "                    next,\n                    retry_hash,\n",
        "                    next,\n                    &retry_hash[..0],\n",
        ("--test", "session_refresh_postgres", "handler_through_production_store_preserves_retry_secret_isolation"),
        "handler_through_production_store_preserves_retry_secret_isolation",
    ),
    (
        "documented media type",
        HANDLERS,
        'pub const BOUND_REFRESH_CONTENT_TYPE: &str = "application/vnd.nntune.session-refresh-bound+json";',
        'pub const BOUND_REFRESH_CONTENT_TYPE: &str = "application/vnd.nntune.incorrect+json";',
        ("--lib", "refresh_transport_"),
        "refresh_transport_marker_prevents_empty_upload_downgrade",
    ),
    (
        "media type case folding",
        HANDLERS,
        ".eq_ignore_ascii_case(BOUND_REFRESH_CONTENT_TYPE)",
        "== BOUND_REFRESH_CONTENT_TYPE",
        ("--lib", "refresh_transport_"),
        (
            "refresh_transport_marker_prevents_empty_upload_downgrade",
            "refresh_transport_accepts_each_marker_spelling_for_recovery",
        ),
    ),
    (
        "media type parameter split",
        HANDLERS,
        "value.split(';').next()",
        "Some(value)",
        ("--lib", "refresh_transport_"),
        (
            "refresh_transport_marker_prevents_empty_upload_downgrade",
            "refresh_transport_accepts_each_marker_spelling_for_recovery",
        ),
    ),
    (
        "media type whitespace trim",
        HANDLERS,
        "            media_type\n                .trim()\n                .eq_ignore_ascii_case(BOUND_REFRESH_CONTENT_TYPE)",
        "            media_type.eq_ignore_ascii_case(BOUND_REFRESH_CONTENT_TYPE)",
        ("--lib", "refresh_transport_"),
        (
            "refresh_transport_marker_prevents_empty_upload_downgrade",
            "refresh_transport_accepts_each_marker_spelling_for_recovery",
        ),
    ),
    (
        "runtime header forwarding",
        WORKER,
        "            refresh_content_type.as_deref(),\n",
        "            refresh_content_type.as_deref().filter(|_| false),\n",
        ("--lib", "refresh_transport_"),
        "refresh_transport_requires_marked_recovery_but_preserves_legacy_empty_refresh",
    ),
    (
        "block-commented transport guard",
        WORKER,
        "    if let Some(rejection) = handlers::refresh_transport_rejection(&request, refresh_content_type) {\n        return rejection;\n    }\n",
        "    /*\n    if let Some(rejection) = handlers::refresh_transport_rejection(&request, refresh_content_type) {\n        return rejection;\n    }\n    */\n",
        ("--lib", "refresh_transport_"),
        "refresh_transport_marker_prevents_empty_upload_downgrade",
    ),
)


def run(args):
    prefix = (["cargo"] if os.environ.get("GITHUB_ACTIONS") == "true" else
              ["python3", "scripts/cargo_cache.py", "run", "--"])
    return subprocess.run(
        prefix + ["test", "--locked", "--manifest-path", "workers/api/Cargo.toml",
                  "--features", "postgres-tests", *args, "--", "--nocapture"],
        cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        timeout=180,
    )


def failed_tests(stdout):
    """Names in libtest's closing `failures:` list.

    Under --nocapture a test's panic text lands between `test <name> ... ` and
    `FAILED` (from its own thread, or a parallel one), so the per-test result
    line is unreliable. The closing list is printed after every test finished.
    """
    lines = stdout.splitlines()
    starts = [i for i, line in enumerate(lines) if line == "failures:"]
    if not starts:
        return set()
    names = set()
    for line in lines[starts[-1] + 1:]:
        if not line.strip():
            if names:
                break
            continue
        if not line.startswith("    "):
            break
        names.add(line.strip())
    return names


def killed_by(name, failed):
    return any(test == name or test.endswith(f"::{name}") for test in failed)


def main():
    for label, path, old, new, args, expected in CASES:
        expected_names = (expected,) if isinstance(expected, str) else expected
        original = path.read_text()
        if original.count(old) != 1:
            raise RuntimeError(f"{label}: mutation anchor must occur exactly once")
        baseline = run(args)
        if (baseline.returncode != 0
                or any(name not in baseline.stdout for name in expected_names)
                or "test result: ok." not in baseline.stdout):
            raise RuntimeError(f"{label}: baseline did not pass\n{baseline.stdout[-6000:]}")
        try:
            path.write_text(original.replace(old, new, 1))
            mutant = run(args)
            failed = failed_tests(mutant.stdout)
            if (mutant.returncode != 101 or "test result: FAILED." not in mutant.stdout
                    or not all(killed_by(name, failed) for name in expected_names)):
                raise RuntimeError(f"{label}: expected assertion did not kill mutant\n{mutant.stdout[-6000:]}")
        finally:
            path.write_text(original)
        print(f"refresh-retry-mutants: killed {label} (assertion failure; source restored)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
