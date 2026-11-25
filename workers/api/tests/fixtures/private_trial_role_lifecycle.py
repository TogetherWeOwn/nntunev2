"""End-to-end regression against a disposable PostgreSQL service with synthetic logins."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / ".github/scripts"))

from apply_migration import transaction_body
from staging_apply import load_entries
from staging_trial_role_lifecycle import DATABASE, FIXTURE, NAMES, SCOPE_ENV
from staging_trial_schema import compose

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--disposable-port", required=True, type=int)
parser.add_argument("--local-data-dir", type=Path)
args = parser.parse_args()
port = args.disposable_port
password = "ci-throwaway"
base = dict(PATH=os.environ["PATH"], PGHOST="127.0.0.1", PGPORT=str(port), PGPASSWORD=password,
            PGUSER="postgres", PGDATABASE="nntune_ci", PGSSLMODE="disable")
owner, executor = "lifecycle_fixture_owner", "lifecycle_fixture_executor"
reader, writer = NAMES


def sql(text, *, database=DATABASE):
    result = subprocess.run(["psql", "-XqAtw", "-v", "ON_ERROR_STOP=1"], text=True,
                            input=text, env={**base, "PGDATABASE": database}, capture_output=True, check=False)
    if result.returncode:
        raise AssertionError(result.stderr)
    return result.stdout.strip()


cluster_query = "SELECT system_identifier FROM pg_catalog.pg_control_system();"
container = os.environ.get("TRIAL_FIXTURE_CONTAINER", "")
if args.local_data_dir is not None:
    data_dir = args.local_data_dir.resolve(strict=True)
    if not re.fullmatch(r"/tmp/nntune-trial77f\.[A-Za-z0-9]+/db", str(data_dir)):
        raise SystemExit("requires a scoped disposable local PostgreSQL directory")
    control = subprocess.check_output(["pg_controldata", str(data_dir)], text=True, timeout=10,
                                      env={"PATH": base["PATH"], "LC_ALL": "C"})
    expected_cluster = re.search(r"^Database system identifier:\s+([0-9]+)$", control, re.MULTILINE)
    expected_cluster = expected_cluster[1] if expected_cluster else ""
elif (os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"
      and os.environ.get("GITHUB_JOB") == "api-migrations" and port == 5432
      and re.fullmatch(r"[0-9a-f]{64}", container)):
    expected_cluster = subprocess.check_output(
        ["docker", "exec", container, "psql", "-XAtw", "-U", "postgres", "-d", "nntune_ci", "-c", cluster_query],
        text=True, timeout=10,
    ).strip()
else:
    raise SystemExit("requires independently pinned disposable PostgreSQL")
if not re.fullmatch(r"[0-9]+", expected_cluster) or sql(cluster_query, database="nntune_ci") != expected_cluster:
    raise SystemExit("loopback server does not match the disposable PostgreSQL cluster")

owner_setup = f"""
CREATE ROLE {owner} LOGIN CREATEROLE NOINHERIT PASSWORD '{password}';
CREATE ROLE {executor} LOGIN NOINHERIT PASSWORD '{password}';
GRANT pg_signal_backend TO {owner} WITH INHERIT TRUE;
CREATE DATABASE {DATABASE} OWNER {owner} TEMPLATE template0;
"""
sql(owner_setup, database="nntune_ci")
try:
    sql(f"""
    COMMENT ON DATABASE {DATABASE} IS '{FIXTURE}';
    REVOKE CONNECT, TEMP ON DATABASE nntune_ci, postgres, template1, {DATABASE} FROM PUBLIC;
    GRANT CONNECT, CREATE ON DATABASE {DATABASE} TO {executor};
    ALTER SCHEMA public OWNER TO {executor};
    SET ROLE {owner};
    CREATE ROLE {reader} LOGIN NOINHERIT PASSWORD '{password}';
    CREATE ROLE {writer} LOGIN NOINHERIT PASSWORD '{password}';
    """)
    entries = [e for e in load_entries(ROOT) if e["target"] == "app"]
    migrations = [(ROOT / "migrations" / e["file"]).read_text() for e in entries]
    ledger = ", ".join(f"('{e['file']}','{e['sha256']}')" for e in entries)
    sql(f"SET ROLE {executor};\n" + "\n".join(transaction_body(text) for text in migrations)
        + "CREATE TABLE public.schema_migrations(file text, sha256 text);"
        + f"INSERT INTO public.schema_migrations VALUES {ledger};")
    assert sql(f"SELECT has_table_privilege('{owner}','public.schema_migrations','SELECT');") == "f"

    def role_oid(role):
        return int(sql(f"SELECT oid FROM pg_roles WHERE rolname = '{role}';"))

    database_oid = int(sql("SELECT oid FROM pg_database WHERE datname = current_database();"))
    scope = {"database_oid": database_oid, "issuer_oid": role_oid(owner),
             "reader": [reader, role_oid(reader)], "writer": [writer, role_oid(writer)]}

    def cmd(action, *, success=True, pin=database_oid, **overrides):
        env = {"PATH": base["PATH"], SCOPE_ENV: json.dumps(scope)}
        for kind, login in (("OWNER", owner), ("EXECUTOR", executor), ("READER", reader), ("WRITER", writer)):
            env[f"PRIVATE_TRIAL_{kind}_URL"] = f"postgresql://{login}:{password}@127.0.0.1:{port}/{DATABASE}"
        result = subprocess.run([sys.executable, str(ROOT / ".github/scripts/staging_trial_role_lifecycle.py"),
                                 action, "--fixture-only-oid", str(pin)], env=env | overrides,
                                text=True, capture_output=True, check=False)
        assert password not in (result.stdout + result.stderr)
        assert (result.returncode == 0) == success, result.stdout + result.stderr
        return result.stdout

    cmd("grant", success=False, pin=16500)
    sql(f"COMMENT ON DATABASE {DATABASE} IS NULL;")
    cmd("grant", success=False)
    sql(f"COMMENT ON DATABASE {DATABASE} IS '{FIXTURE}';")
    assert sql(f"SELECT has_database_privilege('{writer}',current_database(),'CONNECT');") == "f"
    sql("GRANT CONNECT ON DATABASE template1 TO PUBLIC;")
    cmd("grant", success=False)
    sql("REVOKE CONNECT ON DATABASE template1 FROM PUBLIC;")
    sql(f"GRANT UPDATE (id) ON public.users TO {writer};")
    cmd("grant", success=False)
    assert sql(f"SELECT has_database_privilege('{writer}',current_database(),'CONNECT');") == "f"
    sql(f"REVOKE UPDATE (id) ON public.users FROM {writer};")
    sql("CREATE FUNCTION public.unreviewed() RETURNS int LANGUAGE sql AS 'SELECT 1';")
    cmd("grant", success=False)
    sql("DROP FUNCTION public.unreviewed();")
    sql(f"GRANT CONNECT ON DATABASE postgres TO {reader};")
    cmd("grant", success=False)
    sql(f"REVOKE CONNECT ON DATABASE postgres FROM {reader};")
    sql("ALTER TABLE public.storage_validated_digest_winners DISABLE TRIGGER storage_validated_digest_winner_immutable;")
    cmd("grant", success=False)
    sql("ALTER TABLE public.storage_validated_digest_winners ENABLE TRIGGER storage_validated_digest_winner_immutable;")

    # Owner CONNECT succeeds, executor transaction rolls back, then grant resumes.
    sql(f"ALTER TABLE public.tracks OWNER TO {owner};")
    report = cmd("grant", success=False)
    assert "staging CONNECT passed" in report and "application grants passed" not in report
    assert sql(f"SELECT has_column_privilege('{writer}','public.users','id','SELECT');") == "f"
    sql(f"ALTER TABLE public.tracks OWNER TO {executor};")
    report = cmd("grant")
    assert f"routed reader {reader} {reader} {scope['reader'][1]} passed" in report
    assert f"routed writer {writer} {writer} {scope['writer'][1]} passed" in report
    cmd("grant")
    cmd("verify", success=False, PRIVATE_TRIAL_READER_URL=f"postgresql://{reader}:wrong@127.0.0.1:{port}/{DATABASE}")
    cmd("verify")

    sql(f"""
    INSERT INTO public.users (id,email,created_at) VALUES ('fixture','fixture@example.invalid',0);
    SET ROLE {writer};
    DO $fixture$
    BEGIN
        PERFORM id FROM public.users WHERE id = 'fixture' FOR SHARE;
        IF NOT FOUND THEN RAISE EXCEPTION 'fixture user was not locked'; END IF;
        INSERT INTO public.tracks(release_id,medium_no,track_no,mbid,title,duration_ms)
          VALUES ('10000000-0000-4000-8000-000000000001',1,1,'30000000-0000-4000-8000-000000000001','Fixture',14000);
        PERFORM id FROM public.tracks WHERE track_no = 1 FOR NO KEY UPDATE;
        IF NOT FOUND THEN RAISE EXCEPTION 'fixture track was not locked'; END IF;
        IF (SELECT duration_ms FROM public.tracks WHERE track_no = 1) <> 14000 THEN
            RAISE EXCEPTION 'fixture duration was not recorded';
        END IF;
        BEGIN
            UPDATE public.tracks SET duration_ms=14000 WHERE false;
            RAISE EXCEPTION 'duration UPDATE escaped its ACL';
        EXCEPTION WHEN insufficient_privilege THEN NULL;
        END;
        BEGIN
            UPDATE public.tracks SET id=id WHERE false;
            RAISE EXCEPTION 'lock-only UPDATE escaped its trigger';
        EXCEPTION WHEN insufficient_privilege THEN NULL;
        END;
        BEGIN
            UPDATE public.storage_validated_digest_winners SET object_id=object_id WHERE false;
            RAISE EXCEPTION 'winner identity UPDATE escaped its trigger';
        EXCEPTION WHEN check_violation THEN NULL;
        END;
    END $fixture$;
    """)
    sql(f"GRANT SELECT ON public.tracks TO {reader};")
    cmd("verify", success=False)
    sql(f"REVOKE SELECT ON public.tracks FROM {reader};")
    cmd("grant")
    sql("ALTER TABLE public.tracks DISABLE TRIGGER private_trial_lock_only;")
    cmd("verify", success=False)
    sql("ALTER TABLE public.tracks ENABLE ALWAYS TRIGGER private_trial_lock_only;")
    overlay, _ = compose(ROOT, ROOT, writer, reader, database=DATABASE)
    sql(f"SET ROLE {executor};\n" + overlay)
    cmd("verify", success=False)
    cmd("verify-overlay")

    session = subprocess.Popen(["psql", "-XqAtw", "-v", "ON_ERROR_STOP=1"], text=True, stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                               env={**base, "PGDATABASE": DATABASE, "PGUSER": writer})
    try:
        session.stdin.write("SELECT pg_backend_pid();\n")
        session.stdin.flush()
        assert session.stdout.readline().strip().isdigit()
        report = cmd("retire", success=False)  # Overlay ACLs stay with their owner.
        assert "staging drain passed" in report and "application withdrawal passed" not in report
        session.stdin.write("SELECT 1;\n")
        session.stdin.flush()
        assert session.wait(timeout=10) != 0
    finally:
        if session.poll() is None:
            session.terminate()
            session.wait(timeout=10)

    assert sql(f"SELECT count(*) FROM pg_roles WHERE rolname IN ('{reader}','{writer}') AND rolcanlogin;") == "0"
    sql("DROP TABLE public.private_library_admissions, public.catalogue_s3_sources, public.storage_s3_locator_intents;")
    sql(f"REVOKE SELECT (title) ON public.tracks FROM {reader};")
    cmd("retire")
    cmd("retire")
    assert sql(f"SELECT count(*) FROM pg_roles WHERE rolname IN ('{reader}','{writer}');") == "0"
    assert sql("SELECT count(*) FROM pg_trigger WHERE tgname = 'private_trial_lock_only';") == "0"
    sql(f"SET ROLE {owner}; CREATE ROLE {reader} LOGIN NOINHERIT; CREATE ROLE {writer} LOGIN NOINHERIT;")
    cmd("retire", success=False)  # A replacement role may never inherit the previous pin.
    scope.update(reader=[reader, role_oid(reader)], writer=[writer, role_oid(writer)])
    sql(f"SET ROLE {owner}; DROP ROLE {reader};")
    cmd("retire")  # A pinned reader already removed and an ungranted writer.
    assert sql(f"SELECT count(*) FROM pg_roles WHERE rolname IN ('{reader}','{writer}');") == "0"
finally:
    sql(f"""
    DROP DATABASE {DATABASE} WITH (FORCE);
    DROP ROLE IF EXISTS {reader}, {writer}, {executor}, {owner};
    GRANT CONNECT, TEMP ON DATABASE nntune_ci, postgres, template1 TO PUBLIC;
    """, database="nntune_ci")
print("Private trial role lifecycle passed on disposable PostgreSQL")
