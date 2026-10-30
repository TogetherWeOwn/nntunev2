#!/usr/bin/env python3
"""Execute enrollment/reaper negative controls in the disposable CI checkout.

Only GitHub-hosted CI may run this script. Each temporary mutation is scoped
strictly to its named production helper, and the original bytes are
restored in finally before running the complete PostgreSQL bank again. A
compiler error, missing test, connection failure or unrelated panic never
counts as a killed mutant. No mutated source is committed or deployed.
"""

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys

from run_atomic_registration_store_tests import PREFIX, ROOT

SOURCE = Path("workers/api/src/worker_entry.rs")
MUTATIONS = (
    (
        "missing-invite-claim",
        "claims.push(SingleUseClaim::Invite { record });",
        "let _ = record;",
        PREFIX + "postgres_registration_commit_consumes_optional_invite",
        "challenge, magic, invite, credential count",
        "async fn complete_registration_on(",
    ),
    (
        "zero-row-claim-accepted",
        "claim_single_use_on(&transaction, claim).await? != 1",
        "claim_single_use_on(&transaction, claim).await? > 1",
        PREFIX + "postgres_registration_zero_row_magic_claim_rolls_back_earlier_challenge",
        "a zero-row later claim must abort enrollment",
        "async fn complete_registration_on(",
    ),
    (
        "missing-enrollment-parent-lock",
        "SELECT id FROM users WHERE id = $1 FOR KEY SHARE",
        "SELECT id FROM users WHERE id = $1",
        PREFIX + "postgres_reap_before_prune_preserves_in_flight_enrollment",
        "user reaper must not wait for the in-flight enrollment",
        "async fn complete_registration_on(",
    ),
    (
        "ignored-protected-user-binding",
        None,
        '    let protected_user_id = protected_user_id.map(|_| "");\n',
        PREFIX + "postgres_reap_preserves_explicit_protected_user_and_dependents",
        "protected signup must survive the production reaper",
        "async fn reap_unverified_users_on(",
    ),
)


def mutated_source(original, old, new, signature="async fn complete_registration_on("):
    text = original.decode("utf-8")
    if signature not in ("async fn complete_registration_on(", "async fn reap_unverified_users_on("):
        raise RuntimeError("negative control must target a named enrollment/reaper helper")
    if text.count(signature) != 1:
        raise RuntimeError("negative control must identify exactly one production helper")
    start = text.index(signature)
    end = text.index("\nasync fn ", start + 1)
    helper = text[start:end]
    if old is None:
        # Inject the type-preserving shadow at entry, regardless of the
        # selected helper's return type or later control-flow braces.
        header, separator, body = helper.partition("{\n")
        if not separator:
            raise RuntimeError("negative control must identify the production helper body")
        changed = header + separator + new + body
    else:
        if helper.count(old) != 1:
            raise RuntimeError("negative control must match exactly once inside the production helper")
        changed = helper.replace(old, new, 1)
    return (text[:start] + changed + text[end:]).encode("utf-8")


def require_assertion_failure(result, target, reason):
    output = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", result.stdout)
    failure = re.search(r"test result: FAILED\. 0 passed; 1 failed; 0 ignored;", output)
    named = re.search(r"(?m)^\s*" + re.escape(target) + r"\s*$", output)
    if result.returncode != 101 or not failure or not named or reason not in output:
        raise RuntimeError(
            f"negative control did not reach its expected assertion: {target}; "
            f"cargo_exit={result.returncode} (compile, infrastructure and unrelated failures do not count)"
        )
    return output


def main(protected_only=False):
    if (
        os.environ.get("GITHUB_ACTIONS") != "true"
        or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"
    ):
        sys.exit("negative controls require the ephemeral GitHub-hosted CI checkout")
    if not os.environ.get("NNTUNE_TEST_DATABASE_URL"):
        sys.exit("NNTUNE_TEST_DATABASE_URL is required; negative controls cannot skip")
    controls = MUTATIONS
    if protected_only:
        controls = tuple(control for control in MUTATIONS if control[0] == "ignored-protected-user-binding")
        if len(controls) != 1:
            raise RuntimeError("the protected-user negative control must be present exactly once")
    path = ROOT / SOURCE
    original = path.read_bytes()
    try:
        for name, old, new, target, reason, signature in controls:
            if path.read_bytes() != original:
                raise RuntimeError("production source changed between negative controls")
            try:
                path.write_bytes(mutated_source(original, old, new, signature))
                result = subprocess.run(
                    [
                        "cargo", "test", "--locked", "--manifest-path", "workers/api/Cargo.toml",
                        "--lib", target, "--", "--ignored", "--exact", "--nocapture", "--test-threads=1",
                    ],
                    cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                    timeout=180, check=False,
                )
                output = require_assertion_failure(result, target, reason)
                print(f"mutation killed: {name}; cargo_exit={result.returncode}; test={target}", flush=True)
                for line in output.splitlines():
                    if reason in line or line.startswith("test result:") or line.startswith(("  left:", " right:")):
                        print(line, flush=True)
            finally:
                path.write_bytes(original)
    finally:
        path.write_bytes(original)
        if path.read_bytes() != original:
            raise RuntimeError("failed to restore the original production source")
    if protected_only:
        target = controls[0][3]
        print("mutation controls: production source restored byte-for-byte; rerunning protected-user case", flush=True)
        result = subprocess.run(
            [
                "cargo", "test", "--locked", "--manifest-path", "workers/api/Cargo.toml",
                "--lib", target, "--", "--ignored", "--exact", "--nocapture", "--test-threads=1",
            ],
            cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            timeout=180, check=False,
        )
        output = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", result.stdout)
        if (
            result.returncode != 0
            or "test result: ok. 1 passed; 0 failed; 0 ignored;" not in output
            or not re.search(r"(?m)^test " + re.escape(target) + r" \.\.\. ok$", output)
        ):
            raise RuntimeError("restored protected-user case did not execute exactly once and pass\n" + output)
        print(output, flush=True)
        return
    print("mutation controls: production source restored byte-for-byte; rerunning GREEN", flush=True)
    subprocess.run(
        [sys.executable, "workers/api/tests/run_atomic_registration_store_tests.py"],
        cwd=ROOT, timeout=240, check=True,
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--protected-only", action="store_true",
        help="prove the protected-user control and restored case even while the batch bank is RED",
    )
    main(protected_only=parser.parse_args().protected_only)
