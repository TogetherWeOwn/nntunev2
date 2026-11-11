#!/usr/bin/env python3
"""Agent-dispatchable staging migration apply driver.

Called by .github/workflows/migrations-staging-apply.yml. The workflow
checks out two roots: ``--data-root`` (the dispatch-supplied ref, which
supplies only migration bytes, the manifest and migration SQL) and
``--trusted-root`` (the workflow's own ref, which supplies this driver,
apply_migration.py and migration_history.py). Only the trusted root's
Python code is ever executed; data-root code is never imported or run,
so a dispatch-supplied ref cannot hand the migrator credential to
attacker code. The credential is read only from the job environment
inside the workflow job, normalized to sslmode=verify-full, and passed
to the trusted migrator through an allowlisted child environment.

Modes: ``single`` applies one manifest-checked ``app`` entry; refusal of
a non-``app`` target happens before connecting. ``all-pending`` walks
the app entries up to the input file in manifest order and stops at the
first failure, reporting the applied prefix as evidence.
"""
import argparse
import os
import re
import subprocess
import sys
import urllib.parse
from pathlib import Path

import migration_history

FILE_RE = re.compile(r"^\d{4}_[A-Za-z0-9_]+\.sql$")
SECRET_ENV = "PLANETSCALE_MIGRATOR_URL_STAGING"
TRUSTED_DIRNAME = ".github/scripts"
# Names a trusted-code child is allowed to inherit. DATABASE_URL is
# added explicitly; everything else (including any dispatcher-supplied
# environment) is dropped before the migrator runs.
CHILD_ENV_ALLOWLIST = ("PATH", "HOME", "LANG", "LC_ALL", "SSL_CERT_FILE", "SSL_CERT_DIR")


class StagingApplyError(Exception):
    pass


def _env(environ):
    return os.environ if environ is None else environ


def normalize_url(raw, expected_database):
    """Validate the migrator URL and force sslmode=verify-full.

    Shared by the apply and evidence phases so both accept exactly the
    same secrets: whatever sslmode the secret carries (or omits), the
    connection is verify-full in both.
    """
    if not raw:
        raise StagingApplyError("staging migrator secret is missing")
    url = urllib.parse.urlsplit(raw)
    database = urllib.parse.unquote(url.path.removeprefix("/"))
    if (url.scheme not in ("postgres", "postgresql") or not url.hostname
            or not url.username or not database or url.fragment):
        raise StagingApplyError("staging migrator URL is malformed")
    if database != expected_database:
        raise StagingApplyError("staging migrator URL does not target the expected database")
    query = urllib.parse.parse_qs(url.query, strict_parsing=True)
    if (set(query) - {"sslmode", "sslrootcert"}
            or len(query.get("sslmode", [])) > 1
            or query.get("sslrootcert", ["system"]) != ["system"]):
        raise StagingApplyError("staging migrator URL carries unsupported connection options")
    query["sslmode"] = ["verify-full"]
    return urllib.parse.urlunsplit(
        (url.scheme, url.netloc, url.path,
         urllib.parse.urlencode(query, doseq=True), ""))


def allowlisted_child_env(database_url, environ=None):
    """Minimal environment for trusted code: allowlisted host names
    plus the derived DATABASE_URL, never the inherited environment."""
    source = _env(environ)
    child = {key: source[key] for key in CHILD_ENV_ALLOWLIST if key in source}
    child["DATABASE_URL"] = database_url
    return child


def load_entries(data_root):
    manifest_path = Path(data_root) / "migrations/manifest.toml"
    return migration_history.parse_manifest(migration_history.read_input(str(manifest_path)))


def entry_names(entry):
    """Every recorded name for an entry: current name plus renamed_from."""
    names = [entry["file"]]
    names += [alias["file"] for alias in entry.get("renamed_from", [])]
    return names


def find_entry(entries, filename):
    for entry in entries:
        if filename in entry_names(entry) or entry["file"] == filename:
            return entry
    return None


def plan_files(entries, migration_file, mode):
    """Return the app files to apply, refusing a non-app target upfront
    (before any connection) in both single and all-pending mode."""
    if not FILE_RE.fullmatch(migration_file):
        raise StagingApplyError("file must be a migration basename")
    app = [entry for entry in entries if entry.get("target") == "app"]
    app_names = [entry["file"] for entry in app]
    if mode == "all-pending":
        if migration_file not in app_names:
            entry = find_entry(entries, migration_file)
            if entry is not None and entry.get("target") != "app":
                raise StagingApplyError("file is not an app migration in the pinned manifest")
            raise StagingApplyError("file is not an app migration in the pinned manifest")
        return app_names[:app_names.index(migration_file) + 1]
    if mode != "single":
        raise StagingApplyError("mode must be single or all-pending")
    entry = find_entry(entries, migration_file)
    if entry is not None and entry.get("target") != "app":
        raise StagingApplyError("file is not an app migration in the pinned manifest")
    if entry is None:
        raise StagingApplyError("file is not listed in the pinned manifest")
    return [migration_file]


def trusted_script_path(trusted_root, name="apply_migration.py"):
    return Path(trusted_root) / TRUSTED_DIRNAME / name


def apply_files(trusted_root, data_root, migration_file, applied_by, mode,
                expected_database, environ=None):
    """Apply the planned files with the trusted migrator.

    Returns the list of applied filenames (the successful prefix). The
    prefix is written to GITHUB_OUTPUT even on first-failure refusal, so
    the evidence phase reports it on a red run.
    """
    source = _env(environ)
    entries = load_entries(data_root)
    files = plan_files(entries, migration_file, mode)
    database_url = normalize_url(source.get(SECRET_ENV, ""), expected_database)
    if (not applied_by or len(applied_by) > 200
            or any(ord(ch) < 0x20 for ch in applied_by)):
        raise StagingApplyError("applied_by must be non-empty printable text")
    child = allowlisted_child_env(database_url, source)
    script = trusted_script_path(trusted_root)
    if not Path(script).is_file():
        raise StagingApplyError("trusted apply_migration.py is missing")
    done = []
    progress_path = source.get("APPLY_PROGRESS", ".apply-progress")
    try:
        for filename in files:
            if not FILE_RE.fullmatch(filename):
                raise StagingApplyError("manifest carries a malformed migration filename")
            # In-flight marker on the shared job filesystem: if the job
            # times out, the evidence step can still name the file whose
            # commit outcome is unknown instead of silently omitting it.
            with open(progress_path, "w") as handle:
                handle.write(filename)
            argv = [sys.executable, "-I", str(script),
                    "--file", str(Path(data_root) / "migrations" / filename),
                    "--manifest", str(Path(data_root) / "migrations/manifest.toml"),
                    "--applied-by", applied_by,
                    "--expect-database", expected_database]
            result = subprocess.run(argv, capture_output=True, text=True,
                                    env=child, timeout=180, cwd=trusted_root)
            # apply_migration.py output is fixed, non-secret text.
            if result.stdout:
                print(result.stdout, end="")
            if result.returncode != 0:
                if result.stderr:
                    print(result.stderr, end="", file=sys.stderr)
                raise StagingApplyError(
                    f"apply refused on the first failure at migrations/{filename}")
            done.append(filename)
    finally:
        output_path = source.get("GITHUB_OUTPUT")
        if output_path:
            with open(output_path, "a") as handle:
                handle.write(f"applied_files={','.join(done)}\n")
    return done


def evidence_child_env(raw, expected_database):
    """Read-only libpq env from the normalized URL; credentials stay
    env-side, never in argv or output."""
    url = urllib.parse.urlsplit(normalize_url(raw, expected_database))
    database = urllib.parse.unquote(url.path.removeprefix("/"))
    url.port  # raises on a malformed port
    return {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": os.environ.get("HOME", "/home/runner"),
        "PGHOST": url.hostname, "PGPORT": str(url.port or 5432),
        "PGDATABASE": database,
        "PGUSER": urllib.parse.unquote(url.username),
        "PGPASSWORD": urllib.parse.unquote(url.password or ""),
        "PGSSLMODE": "verify-full",
        "PGSSLROOTCERT": "/etc/ssl/certs/ca-certificates.crt",
        "PGCONNECT_TIMEOUT": "10",
        "PGOPTIONS": ("-c default_transaction_read_only=on "
                      "-c statement_timeout=30000 -c lock_timeout=10000 "
                      "-c search_path=public,pg_catalog"),
    }


def quote_literal(name):
    return "'" + name.replace("'", "''") + "'"


def read_row(child, entry, filename):
    """Return the recorded row for an entry under any of its names
    (current name or a renamed_from alias), mirroring entry identity."""
    names = entry_names(entry)
    in_list = ", ".join(quote_literal(name) for name in names)
    sql = ("SELECT file, sha256, applied_at, applied_by "
           f"FROM public.schema_migrations WHERE file IN ({in_list});")
    result = subprocess.run(
        ["psql", "--no-psqlrc", "--no-password", "--quiet",
         "--tuples-only", "--no-align", "--field-separator", "\t",
         "--set", "ON_ERROR_STOP=1"],
        input=sql, text=True, capture_output=True, timeout=60, env=child)
    if result.returncode != 0:
        raise StagingApplyError("evidence read failed; connection details redacted")
    return result.stdout.strip()


def run_evidence(data_root, migration_file, applied_files, apply_outcome,
                 expected_database, environ=None):
    source = _env(environ)
    entries = load_entries(data_root)
    requested = migration_file
    files = [f for f in (applied_files or "").split(",") if f]
    if not files:
        files = [requested]
    summary_path = source["GITHUB_STEP_SUMMARY"]
    with open(summary_path, "a") as handle:
        handle.write("### Recorded schema_migrations rows\n")
    progress_path = source.get("APPLY_PROGRESS", ".apply-progress")
    if Path(progress_path).is_file():
        in_flight = Path(progress_path).read_text().strip()
        if (in_flight and in_flight not in files
                and (apply_outcome or "") != "success"):
            with open(summary_path, "a") as handle:
                handle.write(
                    f"- In-flight at failure: `{in_flight}` "
                    "(outcome unknown — reconcile the receipt and effects "
                    "before retrying)\n")
    missing = []
    for filename in files:
        if not FILE_RE.fullmatch(filename):
            raise StagingApplyError("malformed migration filename in evidence list")
    child = evidence_child_env(source.get(SECRET_ENV, ""), expected_database)
    with open(summary_path, "a") as handle:
        for filename in files:
            entry = find_entry(entries, filename)
            if entry is None:
                raise StagingApplyError("file is not listed in the pinned manifest")
            row = read_row(child, entry, filename)
            if not row:
                missing.append(filename)
                continue
            handle.write(f"- `{row}`\n")
    if apply_outcome == "success" and missing:
        raise StagingApplyError(
            "apply succeeded but no recorded row exists for: " + ", ".join(missing))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("phase", choices=("apply", "evidence"))
    parser.add_argument("--trusted-root", default="trusted")
    parser.add_argument("--data-root", default="data")
    parser.add_argument("--file", required=True)
    parser.add_argument("--applied-by", default="")
    parser.add_argument("--mode", default="single", choices=("single", "all-pending"))
    parser.add_argument("--applied-files", default="")
    parser.add_argument("--apply-outcome", default="")
    parser.add_argument("--expect-database", required=True)
    args = parser.parse_args(argv)
    try:
        if args.phase == "apply":
            apply_files(args.trusted_root, args.data_root, args.file,
                        args.applied_by, args.mode, args.expect_database)
        else:
            run_evidence(args.data_root, args.file, args.applied_files,
                         args.apply_outcome, args.expect_database)
    except StagingApplyError as exc:
        print(f"migrations-staging-apply: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
