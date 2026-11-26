"""End-to-end regression against a disposable PostgreSQL service with synthetic logins."""

import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / ".github/scripts"))

from private_trial_catalog import apply_catalog, create_catalog
from staging_apply import StagingApplyError
import staging_trial_ambient_acl as ambient_acl
import staging_trial_role_lifecycle as lifecycle
from staging_trial_isolated import target_config
from staging_trial_role_acl import audit
from staging_trial_role_lifecycle import CONFIG_ENV, DATABASE, FIXTURE, NAMES, SCOPE_ENV
from staging_trial_schema import BASELINE, compose

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


def exercise_catalog_preflight(config, digest):
    env = {
        f"PRIVATE_TRIAL_{key.upper()}_URL": f"postgresql://{config[key]}:{password}@127.0.0.1:{port}/{DATABASE}"
        for key in ("owner", "executor")
    }
    env["PATH"] = base["PATH"]

    def check(*, target=config, failure=None):
        result = subprocess.run(
            [sys.executable, "-B", "-E", "-s", str(ROOT / ".github/scripts/staging_trial_role_lifecycle.py"), "check-catalog",
             "--isolated", "--fixture-only-oid", str(target["database_oid"])],
            env={**env, CONFIG_ENV: json.dumps(target)}, text=True, capture_output=True, timeout=30,
        )
        output = result.stdout + result.stderr
        assert all(value not in output for value in (password, config["owner"], config["executor"], digest))
        if failure is None:
            assert result.returncode == 0 and "isolated catalog preflight passed" in output, output
        else:
            assert result.returncode == 1 and failure in result.stderr, output
            assert "isolated catalog preflight passed" not in output, output

    before = sql(ambient_acl.CATALOG)
    check()
    run_psql = subprocess.run
    # Keep the same disposable identity and catalog while substituting only the version read.
    for login in (owner, executor):
        for version in (180012, 170006):
            def reported_version(*args, input, env, **kwargs):
                if env["PGUSER"] == login:
                    input = input.replace("pg_catalog.current_setting('server_version_num')::int", str(version))
                return run_psql(*args, input=input, env=env, **kwargs)

            with patch.dict(os.environ, env, clear=True), patch.object(subprocess, "run", reported_version):
                try:
                    lifecycle.check_catalog(config, fixture=True)
                except StagingApplyError as error:
                    assert version == 170006 and "expected PostgreSQL 18" in str(error), error
                else:
                    assert version == 180012, f"{login} passed catalog preflight on PostgreSQL 17"

    check(target=config | {"database_oid": 16500}, failure="Trial role lifecycle refused")
    check(target=config | {"database_oid": config["database_oid"] + 1}, failure="isolated owner identity")
    check(target=config | {"executor_oid": config["executor_oid"] + 1}, failure="isolated executor identity")
    sql(f"UPDATE public.schema_migrations SET sha256 = repeat('0',64) WHERE file = '{BASELINE}';")
    check(failure="isolated executor identity or migration baseline")
    sql(f"UPDATE public.schema_migrations SET sha256 = '{digest}' WHERE file = '{BASELINE}';")
    # Use a bundled stand-in for an unexpected provider extension in its own schema.
    sql("CREATE SCHEMA pscale_extensions; CREATE EXTENSION btree_gin WITH SCHEMA pscale_extensions;")
    check(failure="docs/ops/private-staging-trial.md#isolated-database-catalog-preflight")
    assert sql("SELECT extname FROM pg_extension ORDER BY extname;") == "btree_gin\nplpgsql"
    assert sql(ambient_acl.CATALOG) == before
    assert sql(f"SELECT count(*) FROM pg_roles WHERE rolname IN ('{reader}','{writer}');") == "0"
    sql("DROP EXTENSION btree_gin; DROP SCHEMA pscale_extensions;")
    check()
    print("isolated catalog preflight rehearsal passed")


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

create_catalog(sql, "nntune_ci", owner, executor, password)
try:
    sql(f"""
    COMMENT ON DATABASE {DATABASE} IS '{FIXTURE}';
    REVOKE CONNECT, TEMP ON DATABASE nntune_ci, postgres, template1, {DATABASE} FROM PUBLIC;
    """)
    entries = apply_catalog(sql, ROOT, executor)
    assert sql(f"SELECT has_table_privilege('{owner}','public.schema_migrations','SELECT');") == "f"

    def role_oid(role):
        return int(sql(f"SELECT oid FROM pg_roles WHERE rolname = '{role}';"))

    database_oid = int(sql("SELECT oid FROM pg_database WHERE datname = current_database();"))
    config = dict(database_oid=database_oid, executor_oid=role_oid(executor), direct="127.0.0.1",
                  direct_port=port, routed="127.0.0.1", routed_port=port,
                  owner=owner, executor=executor, reader=reader, writer=writer)
    applied = next(e["sha256"] for e in entries if e["file"] == BASELINE)
    exercise_catalog_preflight(config, applied)
    sql(f"""
    SET ROLE {owner};
    CREATE ROLE {reader} LOGIN NOINHERIT PASSWORD '{password}';
    CREATE ROLE {writer} LOGIN NOINHERIT PASSWORD '{password}';
    """)
    scope = {"database_oid": database_oid, "issuer_oid": role_oid(owner),
             "reader": [reader, role_oid(reader)], "writer": [writer, role_oid(writer)]}

    def cmd(action, *, success=True, pin=database_oid, isolated=None, **overrides):
        env = {"PATH": base["PATH"], SCOPE_ENV: json.dumps(scope)}
        for kind, login in (("OWNER", owner), ("EXECUTOR", executor), ("READER", reader), ("WRITER", writer)):
            env[f"PRIVATE_TRIAL_{kind}_URL"] = f"postgresql://{login}:{password}@127.0.0.1:{port}/{DATABASE}"
        argv = [sys.executable, str(ROOT / ".github/scripts/staging_trial_role_lifecycle.py"),
                action, "--fixture-only-oid", str(pin)]
        if isolated is not None:
            env[CONFIG_ENV] = json.dumps(isolated)
            argv.append("--isolated")
        result = subprocess.run(argv, env=env | overrides,
                                text=True, capture_output=True, check=False)
        assert password not in (result.stdout + result.stderr)
        assert (result.returncode == 0) == success, result.stdout + result.stderr
        return result.stdout

    cmd("grant", success=False, pin=16500)
    sql(f"COMMENT ON DATABASE {DATABASE} IS NULL;")
    cmd("grant", success=False)
    sql(f"COMMENT ON DATABASE {DATABASE} IS '{FIXTURE}';")
    assert sql(f"SELECT has_database_privilege('{writer}',current_database(),'CONNECT');") == "f"
    sql(f"UPDATE public.schema_migrations SET sha256 = repeat('0', 64) WHERE file = '{BASELINE}';")
    report = cmd("grant", success=False)
    assert "staging CONNECT passed" not in report
    assert sql(f"""SELECT bool_or(has_database_privilege(rolname,current_database(),'CONNECT'))
                   FROM pg_roles WHERE rolname IN ('{reader}','{writer}');""") == "f"
    sql(f"UPDATE public.schema_migrations SET sha256 = '{applied}' WHERE file = '{BASELINE}';")
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

    # Rehearse the separate branch's split authority. Retain all app objects and
    # data but let the database owner, not the executor, control the schema ACL.
    sql(f"""
    DROP FUNCTION public.check_storage_s3_locator_intent();
    ALTER SCHEMA public OWNER TO pg_database_owner;
    REVOKE CREATE ON SCHEMA public FROM {executor};
    SET ROLE {owner};
    CREATE ROLE {reader} LOGIN NOINHERIT PASSWORD '{password}';
    CREATE ROLE {writer} LOGIN NOINHERIT PASSWORD '{password}';
    """)
    scope.update(reader=[reader, role_oid(reader)], writer=[writer, role_oid(writer)])
    hosted_config = config | {"direct": "direct.fixture.psdb.cloud", "routed": "routed.fixture.psdb.cloud",
                              "owner": "postgres.fixture"}
    assert target_config(json.dumps(hosted_config), scope, fixture=False) == hosted_config
    try:
        shared_pin = {"database_oid": 16500}
        target_config(json.dumps(hosted_config | shared_pin), scope | shared_pin, fixture=False)
    except ValueError:
        pass
    else:
        raise AssertionError("isolated admission accepted the shared database pin")

    def isolated(action, *, config=config, **kwargs):
        return cmd(action, isolated=config, **kwargs)

    def ungranted():
        assert sql(f"""SELECT bool_or(has_database_privilege(rolname,current_database(),'CONNECT'))
                       FROM pg_roles WHERE rolname IN ('{reader}','{writer}');""") == "f"
        assert sql("SELECT to_regclass('public.private_library_admissions') IS NULL;") == "t"

    wrong_oid = database_oid + 1
    isolated("grant", success=False, pin=wrong_oid, config=config | {"database_oid": wrong_oid},
             **{SCOPE_ENV: json.dumps(scope | {"database_oid": wrong_oid})})
    isolated("grant", success=False, config=config | {"executor_oid": role_oid(owner)})
    isolated("grant", success=False,
             PRIVATE_TRIAL_WRITER_URL=f"postgresql://{writer}.wrong:{password}@127.0.0.1:{port}/{DATABASE}")
    isolated("grant", success=False,
             PRIVATE_TRIAL_READER_URL=f"postgresql://{reader}:{password}@127.0.0.2:{port}/{DATABASE}")
    isolated("grant", success=False, config=config | {"routed_port": port - 1})
    isolated("grant", success=False)  # Table ownership does not confer schema CREATE.
    ungranted()
    sql(f"GRANT CREATE ON SCHEMA public TO {executor};")
    isolated("grant", success=False)  # PUBLIC USAGE does not confer a grant option.
    ungranted()
    sql(f"GRANT USAGE ON SCHEMA public TO {executor} WITH GRANT OPTION;")
    signature = "public.lock_storage_digest_winner(uuid,uuid,bytea,bigint,uuid,uuid,uuid)"
    sql(f"ALTER FUNCTION {signature} OWNER TO {owner};")
    isolated("grant", success=False)
    ungranted()
    sql(f"GRANT EXECUTE ON FUNCTION {signature} TO {executor} WITH GRANT OPTION;")
    sql(f"ALTER FUNCTION {signature} STABLE;")
    isolated("grant", success=False)  # Correct ownership or grant option is only half the contract.
    ungranted()
    sql(f"ALTER FUNCTION {signature} VOLATILE;")
    locator_guard = "public.storage_object_intent_append_only()"
    sql(f"ALTER FUNCTION {locator_guard} OWNER TO {owner}; REVOKE EXECUTE ON FUNCTION {locator_guard} FROM PUBLIC;")
    isolated("grant", success=False)
    ungranted()
    sql(f"GRANT EXECUTE ON FUNCTION {locator_guard} TO {executor};")
    sql(f"UPDATE public.schema_migrations SET sha256 = repeat('0',64) WHERE file = '{BASELINE}';")
    isolated("grant", success=False)
    ungranted()
    sql(f"UPDATE public.schema_migrations SET sha256 = '{applied}' WHERE file = '{BASELINE}';")
    sql(f"REVOKE pg_signal_backend FROM {owner};")
    report = isolated("grant", success=False)
    assert "isolated executor authority passed" in report and "staging CONNECT passed" not in report
    ungranted()
    sql(f"GRANT pg_signal_backend TO {owner} WITH INHERIT TRUE;")
    sql("CREATE SCHEMA trial_fixture_extensions;")
    for extension in ("pg_trgm", "dblink", "postgres_fdw"):
        sql(f"CREATE EXTENSION {extension} WITH SCHEMA trial_fixture_extensions;\n"
            "REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA trial_fixture_extensions FROM PUBLIC;")
        sql(audit([], exact=False, database_preparation=True))
        assert "staging CONNECT passed" not in isolated("grant", success=False)
        ungranted()
        sql(f"DROP EXTENSION {extension};")
    sql("DROP SCHEMA trial_fixture_extensions;")

    sql("GRANT TEMP ON DATABASE nntune_ci, template1 TO PUBLIC;")
    assert "isolated owner authority passed" in isolated("check")
    ungranted()
    for catalog in ("nntune_ci", "template1"):
        sql(f"GRANT CONNECT ON DATABASE {catalog} TO PUBLIC;")
        isolated("check", success=False)
        sql(f"REVOKE CONNECT ON DATABASE {catalog} FROM PUBLIC;")
    sql(f"GRANT TEMP ON DATABASE nntune_ci TO {reader};")
    isolated("check", success=False)
    sql(f"REVOKE TEMP ON DATABASE nntune_ci FROM {reader};")
    # A stale or wrong routed password can only be discovered after CONNECT.
    assert "application grants passed" in isolated("grant", success=False,
        PRIVATE_TRIAL_WRITER_URL=f"postgresql://{writer}:wrong@127.0.0.1:{port}/{DATABASE}")
    assert "routed writer passed" in isolated("grant")
    assert "routed reader passed" in isolated("verify-overlay")
    for catalog in ("nntune_ci", "template1"):
        for role in (reader, writer):
            denied = subprocess.run(["psql", "-XqAtw", "-c", "SELECT 1"], capture_output=True, text=True,
                                    env={**base, "PGDATABASE": catalog, "PGUSER": role}, timeout=10)
            assert denied.returncode and "permission denied for database" in denied.stderr
    sql(f"""
    INSERT INTO public.invite_codes(code_hash,created_at,expires_at) VALUES ('isolated-fixture',0,1);
    SET ROLE {writer};
    INSERT INTO public.private_library_admissions(invite_id,recipient_id,track_id,accepted_seq,object_id)
      SELECT i.id, 'fixture', t.id, 1, gen_random_uuid() FROM public.invite_codes i CROSS JOIN public.tracks t;
    """)
    assert sql(f"SET ROLE {reader}; SELECT count(*) FROM public.private_library_admissions;") == "1"
    assert sql(f"SELECT has_table_privilege('{reader}','public.private_library_admissions','INSERT,UPDATE,DELETE');") == "f"
    # A protected main revision can carry a newer recipe than the deployed
    # receipt. Verification refuses that drift and withdrawal can still run.
    previous_digest = "1" * 64
    receipt = f"isolated-private-trial {previous_digest} {scope['reader'][1]} {scope['writer'][1]}"
    sql(f"COMMENT ON TABLE public.private_library_admissions IS '{receipt}';")
    isolated("verify-overlay", success=False)
    # An independent ACL drift refuses retirement after fencing. Repair and
    # retry, leaving the overlay and unrelated application rows intact.
    sql(f"GRANT DELETE ON public.private_library_admissions TO {writer};")
    assert "staging drain passed" in isolated("retire", success=False)
    assert sql(f"SELECT count(*) FROM pg_roles WHERE rolname IN ('{reader}','{writer}') AND rolcanlogin;") == "0"
    sql(f"REVOKE DELETE ON public.private_library_admissions FROM {writer};")
    sql(f"COMMENT ON TABLE public.private_library_admissions IS '{receipt}0';")
    assert "application withdrawal passed" not in isolated("retire", success=False)
    sql(f"COMMENT ON TABLE public.private_library_admissions IS '{receipt}';")
    isolated("retire")
    isolated("retire")
    assert sql(f"SELECT count(*) FROM pg_roles WHERE rolname IN ('{reader}','{writer}');") == "0"
    assert sql("SELECT count(*) FROM pg_trigger WHERE tgname = 'private_trial_lock_only';") == "0"
    assert sql("SELECT count(*) FROM public.users WHERE id = 'fixture';") == "1"
    assert sql("SELECT count(*) FROM public.private_library_admissions;") == "1"
    assert sql("SELECT obj_description('public.private_library_admissions'::regclass) LIKE 'isolated-private-trial %';") == "t"
    sql(f"SET ROLE {owner}; CREATE ROLE {reader} LOGIN NOINHERIT; CREATE ROLE {writer} LOGIN NOINHERIT;")
    scope.update(reader=[reader, role_oid(reader)], writer=[writer, role_oid(writer)])
    assert "staging CONNECT passed" not in isolated("grant", success=False)
    assert sql(f"SELECT has_database_privilege('{writer}',current_database(),'CONNECT');") == "f"

    # Exercise supported ACL-only change and inverse while no trial role exists.
    sql(f"DROP ROLE {reader}, {writer}; ALTER DATABASE {DATABASE} OWNER TO postgres;")
    other = "trial_ambient_fixture"
    provider = "trial_ambient_provider_fixture"
    sql(f"CREATE DATABASE {other} TEMPLATE template0;\nCREATE DATABASE {provider} OWNER {owner} TEMPLATE template0;")
    try:
        sql(f"ALTER DATABASE template1 OWNER TO {owner};")
        sql(f"GRANT CONNECT,TEMP ON DATABASE {DATABASE}, template1 TO PUBLIC;")
        sql(f"REVOKE CONNECT ON DATABASE {provider} FROM PUBLIC;")
        sql(f"GRANT CONNECT ON DATABASE template1 TO {executor} WITH GRANT OPTION;")
        initial = json.loads(sql(ambient_acl.CATALOG))
        assert sql(f"SELECT datacl IS NULL FROM pg_database WHERE datname='{other}';") == "t"
        template_oid = next(row[0] for row in initial["databases"] if row[1] == "template1")
        consumer = [[template_oid, role_oid(executor), executor, "CONNECT"],
                    [template_oid, role_oid(executor), executor, "TEMPORARY"]]

        plan = dict(branch_id="fixture", direct="direct.fixture.psdb.cloud", port=5432,
                    baseline=initial, consumers=consumer)
        db_meta = {"default_branch": "main"}
        branch_meta = {"id": "fixture", "name": "staging", "ready": True, "production": False}
        url = f"postgresql://postgres.fixture:{password}@{plan['direct']}:{plan['port']}/{DATABASE}"

        def entrypoint(*, config=plan, owner_url=url, checksum=None, token="fixture:token",
                       database=db_meta, branch=branch_meta, branch_status=200, accepted=False):
            raw = json.dumps(config)
            env = dict(AMBIENT_PLAN=raw, AMBIENT_PLAN_SHA256=checksum or hashlib.sha256(raw.encode()).hexdigest(),
                       PRIVATE_TRIAL_OWNER_URL=owner_url)
            if token is not None:
                env["ISOLATED_TRIAL_BRANCH_READ"] = token
            conn = Mock()
            conn.getresponse.side_effect = [Mock(status=200, read=Mock(return_value=json.dumps(database).encode())),
                                            Mock(status=branch_status, read=Mock(return_value=json.dumps(branch).encode()))]
            with (patch.dict(os.environ, env, clear=True), patch.object(sys, "argv", ["fixture", "check-ambient"]),
                  patch.object(ambient_acl, "DATABASE_OID", database_oid),
                  patch.object(ambient_acl.http.client, "HTTPSConnection", return_value=conn),
                  patch.object(ambient_acl, "psql") as psql):
                assert (ambient_acl.main() == 0) == accepted
                if accepted:
                    psql.assert_called_once()
                    assert conn.request.call_count == 2 and token not in psql.call_args.args[0].values()
                else:
                    psql.assert_not_called()

        entrypoint(accepted=True)
        for invalid in (
            dict(checksum="0" * 64), dict(config=plan | {"extra": True}),
            dict(owner_url=url.replace(plan["direct"], "wrong.fixture.psdb.cloud")),
            dict(owner_url=url.replace(":5432/", ":6432/")), dict(owner_url=url.replace("postgres.", "other.")),
            dict(config=plan | {"port": 6432}, owner_url=url.replace(":5432/", ":6432/")),
            dict(database={"default_branch": "staging"}), dict(branch=branch_meta | {"id": "other"}),
            dict(branch=branch_meta | {"production": True}), dict(branch_status=503),
            dict(token=None), dict(token="malformed"),
        ):
            entrypoint(**invalid)

        def ambient(action, snapshot=initial):
            return ambient_acl.compose(action, snapshot, consumer, database_oid, "postgres", fixture=True)

        def refused(statement):
            try:
                sql(statement)
            except AssertionError:
                return
            raise AssertionError("ambient ACL operation accepted an unsafe plan")

        sql(ambient("check-ambient"))
        assert json.loads(sql(ambient_acl.CATALOG)) == initial
        sql(f"GRANT CONNECT ON DATABASE {provider} TO PUBLIC;")
        try:
            ambient("restrict-ambient", snapshot=json.loads(sql(ambient_acl.CATALOG)))
        except StagingApplyError as error:
            assert str(error) == "isolated database ACL grantor authority"
        else:
            raise AssertionError("ambient ACL operation accepted a foreign grantor")
        sql(f"REVOKE CONNECT ON DATABASE {provider} FROM PUBLIC;")
        assert json.loads(sql(ambient_acl.CATALOG)) == initial
        sql("CREATE FUNCTION public.ambient_fixture_unexpected() RETURNS int LANGUAGE sql AS 'SELECT 1';")
        refused(ambient("restrict-ambient"))
        assert json.loads(sql(ambient_acl.CATALOG)) == initial
        sql("DROP FUNCTION public.ambient_fixture_unexpected();")
        forward = ambient("restrict-ambient")
        reliant = "ambient_fixture_reliant"
        sql(f"CREATE ROLE {reliant} LOGIN PASSWORD '{password}';")

        @contextmanager
        def public_client():
            session = subprocess.Popen(["psql", "-XqAtw"], text=True, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                       stderr=subprocess.DEVNULL,
                                       env={**base, "PGDATABASE": "template1", "PGUSER": reliant, "PGCONNECT_TIMEOUT": "5"})
            pid = None
            try:
                session.stdin.write("SELECT pg_backend_pid();\n")
                session.stdin.flush()
                pid = int(session.stdout.readline().strip())
                yield
            finally:
                try:
                    if pid is not None:
                        pinned_backend = f"pid = {pid} AND usename = '{reliant}' AND datname = 'template1'"
                        assert sql(f"SELECT pg_terminate_backend(pid, 5000) FROM pg_stat_activity WHERE {pinned_backend};") in ("", "t")
                        assert sql(f"SELECT NOT EXISTS (SELECT FROM pg_stat_activity WHERE {pinned_backend});") == "t"
                finally:
                    if session.poll() is None:
                        session.terminate()
                    session.wait(timeout=10)

        try:
            with public_client():
                refused(forward)
                assert json.loads(sql(ambient_acl.CATALOG)) == initial

            # Wait for the first session scan, then connect while PUBLIC still has CONNECT.
            prefix, boundary, rest = forward.partition("\\if :change_needed\n")
            assert boundary
            cutover = subprocess.Popen(["psql", "-XqAtw", "-v", "ON_ERROR_STOP=1"], text=True,
                                       stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                       env={**base, "PGDATABASE": DATABASE, "PGCONNECT_TIMEOUT": "5"})
            try:
                cutover.stdin.write(prefix + "SELECT 'precheck passed';\n")
                cutover.stdin.flush()
                assert any(line.strip() == "precheck passed" for line in cutover.stdout)
                with public_client():
                    _, error = cutover.communicate(boundary + rest, timeout=15)
                    assert cutover.returncode != 0 and "contract refused" in error
                    assert json.loads(sql(ambient_acl.CATALOG)) == initial
            finally:
                if cutover.poll() is None:
                    cutover.terminate()
                cutover.wait(timeout=10)
        finally:
            sql(f"DROP ROLE {reliant};")
        changed = forward.replace("\\gexec\n", "\\gexec\nSELECT 1/0;\n", 1)
        assert changed != forward
        refused(changed)
        assert json.loads(sql(ambient_acl.CATALOG)) == initial
        sql(forward)
        restricted = json.loads(sql(ambient_acl.CATALOG))
        db_names = {entry[0]: entry[1] for entry in restricted["databases"]}
        assert {(db_names[d], p) for d, grantee, _, p, _ in restricted["acl"] if grantee == 0} == {
            ("nntune_ci", "TEMPORARY"), (provider, "TEMPORARY"),
        }
        assert sql(f"SELECT has_database_privilege('{executor}','template1','CONNECT WITH GRANT OPTION')"
                   f" AND has_database_privilege('{executor}','template1','TEMPORARY');") == "t"
        sql(forward)
        assert json.loads(sql(ambient_acl.CATALOG)) == restricted
        sql(f"CREATE ROLE {reader} NOLOGIN NOINHERIT;")
        refused(ambient("restore-ambient"))
        assert json.loads(sql(ambient_acl.CATALOG)) == restricted
        sql(f"DROP ROLE {reader};")
        sql(ambient("restore-ambient"))
        sql(ambient("restore-ambient"))
        assert json.loads(sql(ambient_acl.CATALOG)) == initial
        assert sql(f"SELECT datacl IS NULL FROM pg_database WHERE datname='{other}';") == "f"
    finally:
        sql(f"DROP DATABASE {other};\nDROP DATABASE {provider};")
    print("Isolated ambient ACL prepare, refusal, retry and restoration passed")
finally:
    sql(f"""
    DROP DATABASE {DATABASE} WITH (FORCE);
    ALTER DATABASE template1 OWNER TO postgres;
    REVOKE CONNECT, TEMP ON DATABASE template1 FROM {executor};
    DROP ROLE IF EXISTS {reader}, {writer}, {executor}, {owner};
    GRANT CONNECT, TEMP ON DATABASE nntune_ci, postgres, template1 TO PUBLIC;
    """, database="nntune_ci")
print("Private trial role lifecycle passed on disposable PostgreSQL")
