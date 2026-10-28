#!/usr/bin/env python3
"""Execute two enrollment negative controls in the disposable CI checkout.

Only GitHub-hosted CI may run this script. Each temporary mutation is scoped
strictly to the production transaction helper, and the original bytes are
restored in finally before running the complete PostgreSQL bank again. A
compiler error, missing test, connection failure or unrelated panic never
counts as a killed mutant. No mutated source is committed or deployed.
"""

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
    ),
    (
        "zero-row-claim-accepted",
        "claim_single_use_on(&transaction, claim).await? != 1",
        "claim_single_use_on(&transaction, claim).await? > 1",
        PREFIX + "postgres_registration_zero_row_magic_claim_rolls_back_earlier_challenge",
        "a zero-row later claim must abort enrollment",
    ),
)


def mutated_source(original, old, new):
    text = original.decode("utf-8")
    start = text.index("async fn complete_registration_on(")
    end = text.index("\nasync fn ", start + 1)
    transaction = text[start:end]
    if transaction.count(old) != 1:
        raise RuntimeError("negative control must match exactly once inside the production transaction")
    return (text[:start] + transaction.replace(old, new, 1) + text[end:]).encode("utf-8")


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


def main():
    if (
        os.environ.get("GITHUB_ACTIONS") != "true"
        or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"
    ):
        sys.exit("negative controls require the ephemeral GitHub-hosted CI checkout")
    if not os.environ.get("NNTUNE_TEST_DATABASE_URL"):
        sys.exit("NNTUNE_TEST_DATABASE_URL is required; negative controls cannot skip")
    path = ROOT / SOURCE
    original = path.read_bytes()
    try:
        for name, old, new, target, reason in MUTATIONS:
            if path.read_bytes() != original:
                raise RuntimeError("production source changed between negative controls")
            try:
                path.write_bytes(mutated_source(original, old, new))
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
    print("mutation controls: production source restored byte-for-byte; rerunning GREEN", flush=True)
    subprocess.run(
        [sys.executable, "workers/api/tests/run_atomic_registration_store_tests.py"],
        cwd=ROOT, timeout=240, check=True,
    )


if __name__ == "__main__":
    main()
