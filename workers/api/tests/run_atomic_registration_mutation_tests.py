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
SUPPORT = Path("workers/api/tests/support/atomic_registration_store_acceptance.rs")
TOP_LEVEL_HELPERS = (
    "async fn complete_registration_on(",
    "async fn reap_unverified_users_on(",
)
IMPL_HELPERS = (
    "    async fn insert_magic(",
)
SUPPORT_HELPERS = (
    "fn validate_disposable_database_config(",
)
PGSTORE_ACTOR = "impl Store for PgStore"
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
    (
        "missing-magic-skip-locked",
        "FOR UPDATE SKIP LOCKED",
        "FOR UPDATE",
        PREFIX + "postgres_magic_issuance_skips_locked_expired_proof_and_retries",
        "magic issuance must not wait for locked expired cleanup",
        "    async fn insert_magic(",
    ),
    (
        "reaper-releases-selected-parent-lock",
        "        .collect::<Result<Vec<_>, _>>()?;\n"
        '    #[cfg(all(test, not(target_arch = "wasm32")))]\n'
        "    reaper_selection_pause().await;",
        "        .collect::<Result<Vec<_>, _>>()?;\n"
        "    transaction.commit().await.map_err(|_| StoreError)?;\n"
        "    let transaction = client.build_transaction()"
        ".isolation_level(tokio_postgres::IsolationLevel::ReadCommitted)"
        ".start().await.map_err(|_| StoreError)?;\n"
        '    #[cfg(all(test, not(target_arch = "wasm32")))]\n'
        "    reaper_selection_pause().await;",
        PREFIX + "postgres_reap_holds_selected_parent_lock_through_delete",
        "reaper must retain selected parent lock through delete",
        "async fn reap_unverified_users_on(",
    ),
    (
        "allowed-hostaddr-override",
        '    if !config.get_hostaddrs().is_empty() {\n'
        '        return Err("hostaddr overrides are not permitted for the disposable test service");\n'
        "    }\n",
        "",
        PREFIX + "disposable_database_config_rejects_hostaddr_overrides",
        "hostaddr override must be rejected before connecting",
        "fn validate_disposable_database_config(",
        SUPPORT,
        False,
    ),
)


def _parse_control(control):
    if len(control) == 6:
        name, old, new, target, reason, signature = control
        return name, old, new, target, reason, signature, SOURCE, True
    if len(control) == 7:
        name, old, new, target, reason, signature, path = control
        path = path if isinstance(path, Path) else Path(path)
        return name, old, new, target, reason, signature, path, True
    if len(control) == 8:
        name, old, new, target, reason, signature, path, ignored = control
        path = path if isinstance(path, Path) else Path(path)
        return name, old, new, target, reason, signature, path, bool(ignored)
    raise RuntimeError("negative control must carry name, site, target, reason, helper and path")


def _bound_top_level(text, signature):
    if text.count(signature) != 1:
        raise RuntimeError("negative control must identify exactly one production helper")
    start = text.index(signature)
    end = text.index("\nasync fn ", start + 1)
    return start, end


def _bound_impl_method(text, signature):
    header = PGSTORE_ACTOR + " {\n"
    if text.count(PGSTORE_ACTOR) != 1 or text.count(header) != 1:
        raise RuntimeError("negative control must identify exactly one PgStore Store impl")
    actor_start = text.index(header) + len(header)
    actor_end = text.find("\n}", actor_start)
    if actor_end == -1:
        raise RuntimeError("negative control must identify the PgStore Store impl boundary")
    # Rustfmt leaves the impl's closing brace at column zero; method bodies
    # stay indented. Do not follow a same-named method into a later impl.
    actor = text[actor_start:actor_end]
    if actor.count(signature) != 1:
        raise RuntimeError("negative control must identify exactly one production helper in PgStore")
    start = actor_start + actor.index(signature)
    candidates = [actor_end]
    for delimiter in ("\n    async fn ", "\n    fn "):
        found = text.find(delimiter, start + 1, actor_end)
        if found != -1:
            candidates.append(found)
    return start, min(candidates)


def _bound_support_function(text, signature):
    if text.count(signature) != 1:
        raise RuntimeError("negative control must identify exactly one production helper")
    start = text.index(signature)
    candidates = []
    for delimiter in ("\nfn ", "\nasync fn "):
        found = text.find(delimiter, start + 1)
        if found != -1:
            candidates.append(found)
    if candidates:
        return start, min(candidates)
    return start, len(text)


def mutated_source(original, old, new, signature="async fn complete_registration_on("):
    text = original.decode("utf-8")
    if signature in TOP_LEVEL_HELPERS:
        start, end = _bound_top_level(text, signature)
    elif signature in IMPL_HELPERS:
        start, end = _bound_impl_method(text, signature)
    elif signature in SUPPORT_HELPERS:
        start, end = _bound_support_function(text, signature)
    else:
        raise RuntimeError("negative control must target a named enrollment/reaper helper")
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


def _mutant_command(target, ignored):
    command = [
        "cargo", "test", "--locked", "--features", "postgres-tests",
        "--manifest-path", "workers/api/Cargo.toml",
        "--lib", target, "--",
    ]
    if ignored:
        command.append("--ignored")
    command.extend(["--exact", "--nocapture", "--test-threads=1"])
    return command


def _require_restored_single_pass(output, result, target, label):
    cleaned = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", output)
    if (
        result.returncode != 0
        or "test result: ok. 1 passed; 0 failed; 0 ignored;" not in cleaned
        or not re.search(r"(?m)^test " + re.escape(target) + r" \.\.\. ok$", cleaned)
    ):
        raise RuntimeError(f"{label} did not execute exactly once and pass\n" + cleaned)
    return cleaned


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
    parsed = [_parse_control(control) for control in controls]
    needed = []
    for _, _, _, _, _, _, path, _ in parsed:
        if path not in needed:
            needed.append(path)
    originals = {path: (ROOT / path).read_bytes() for path in needed}
    try:
        for name, old, new, target, reason, signature, path, ignored in parsed:
            full = ROOT / path
            if full.read_bytes() != originals[path]:
                raise RuntimeError("production source changed between negative controls")
            try:
                full.write_bytes(mutated_source(originals[path], old, new, signature))
                result = subprocess.run(
                    _mutant_command(target, ignored),
                    cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                    timeout=180, check=False,
                )
                output = require_assertion_failure(result, target, reason)
                print(f"mutation killed: {name}; cargo_exit={result.returncode}; test={target}", flush=True)
                for line in output.splitlines():
                    if reason in line or line.startswith("test result:") or line.startswith(("  left:", " right:")):
                        print(line, flush=True)
            finally:
                full.write_bytes(originals[path])
    finally:
        for path, original in originals.items():
            (ROOT / path).write_bytes(original)
        for path, original in originals.items():
            if (ROOT / path).read_bytes() != original:
                raise RuntimeError("failed to restore the original production source")
    if protected_only:
        target = parsed[0][3]
        print("mutation controls: production source restored byte-for-byte; rerunning protected-user case", flush=True)
        result = subprocess.run(
            _mutant_command(target, True),
            cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            timeout=180, check=False,
        )
        output = _require_restored_single_pass(
            result.stdout, result, target, "restored protected-user case",
        )
        print(output, flush=True)
        return
    native_targets = [target for _, _, _, target, _, _, _, ignored in parsed if not ignored]
    for target in native_targets:
        print("mutation controls: production source restored byte-for-byte; rerunning restored config case", flush=True)
        result = subprocess.run(
            _mutant_command(target, False),
            cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            timeout=180, check=False,
        )
        output = _require_restored_single_pass(
            result.stdout, result, target, "restored config case",
        )
        print(output, flush=True)
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
