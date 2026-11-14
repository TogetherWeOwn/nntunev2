#!/usr/bin/env python3
"""Runtime cases for ``apply_migration.py`` on a disposable local PostgreSQL.

``run(env, test_env)`` is called from ``test_apply_migration_postgres.py``
with the admin libpq env (``env``) and the env of the suite's own fixture
database (``test_env``). Nothing here reuses the suite's database: every
scenario creates its own freshly named database from ``env``, refuses to
reuse an existing one, and drops it again. No remote or deployed
endpoint is contacted; credentials come from ``env`` only.

Covered:

- the frozen mirror migrations, applied in manifest order with the
  repository manifest, match an independent raw ``psql`` apply of the
  same files (catalog fingerprint) and re-running them changes nothing;
- real concurrent CLI applies of one non-idempotent file produce one
  effect and one receipt;
- injected receipt-insert and SQL failures leave the independently
  committed tracking DDL (transaction A) but no effect and no receipt;
- both real psql invocations (A: DDL, B: guarded apply) run with the
  controlled session settings even under hostile parent ``PGOPTIONS``,
  observed from the backend, for apply, ``--record-only`` and
  already-recorded runs;
- application-schema objects with matching routine, catalog, type and
  operator names leave actual normal, record-only and existing-receipt
  paths using the runner-owned tracking table and catalog behavior;
- a wrong ``--expect-database`` that bypasses the URL precheck is
  refused inside the transaction on both A and B;
- server-side statement timeout rollback, a committed apply whose
  response is lost, checksum conflicts retained under a held lock, and
  reconciliation by re-running;
- safe and rejected outer-transaction wrappers through the real CLI
  ``main``;
- no-op re-runs keep read grants and remove non-owner write grants.

All SQL here is benign fixture DDL/DML.
"""
import contextlib
import hashlib
import io
import os
import pathlib
import subprocess
import sys
import tempfile
import time
import urllib.parse
from concurrent.futures import ThreadPoolExecutor
from unittest import mock

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
REAL_MANIFEST = REPO_ROOT / "migrations" / "manifest.toml"

MIRROR_FILES = [
    ("0001_mb_mirror.sql",
     "47309207c2e87761477e4b74884454afd5c68916477be64a9c3bbc2d105723c3"),
    ("0014_mb_import_resume_dlq.sql",
     "8385253fbddb464361f30678cfb8464b7c3d73f7ae5614f0eced870876dcef88"),
    ("0016_mb_track.sql",
     "4e984b85eb9d62a498402dc2ddf9474ac8297fe988b22cbfc1434bd8c7cbdbaa"),
    ("0017_mb_replication_packet_ledger.sql",
     "62ceb24df9c5ea7032fc3cf4e48e5e10f87d909639fa469ed79b4dfcd92009d6"),
    ("0030_mb_mirror_w6b.sql",
     "004fd5068e141c330d45fcc2cf23d48dc5623752b671d0b17fda8f085178b6ef"),
    ("0041_cc0_genre.sql",
     "bada2a0d607632dccb5b66a6bfca72a198f204d42e8ff8dd08f1e4b3a0134140"),
]

ADVISORY_KEY = 7271901
CONTROLLED_SEARCH_PATH = "public,pg_catalog"
# pg_settings order by name: idle_in_transaction_session_timeout,
# lock_timeout, statement_timeout (milliseconds).
CONTROLLED_TIMEOUTS = "90000,10000,90000"

HOSTILE_PGOPTIONS = ("-c search_path=pg_temp -c statement_timeout=1 "
                     "-c lock_timeout=1 -c standard_conforming_strings=off "
                     "-c idle_in_transaction_session_timeout=1")

PROBE = (
    "SELECT 'PROBE|' || pg_catalog.current_setting('search_path') || '|' || "
    "(SELECT pg_catalog.string_agg(setting, ',' ORDER BY name) FROM pg_catalog.pg_settings "
    "WHERE name OPERATOR(pg_catalog.=) ANY (ARRAY["
    "'idle_in_transaction_session_timeout', 'lock_timeout', 'statement_timeout'])) "
    "|| '|' || pg_catalog.current_setting('standard_conforming_strings');\n"
)

FINGERPRINT_SQL = r"""
SELECT md5(string_agg(line, E'\n' ORDER BY line)) || ':' || count(*) FROM (
  SELECT 'rel|' || c.relname || '|' || c.relkind::text AS line
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE n.nspname = 'public' AND c.relname NOT LIKE 'schema_migrations%'
     AND c.relkind IN ('r', 'p', 'v', 'm', 'S', 'i', 'f')
  UNION ALL
  SELECT 'col|' || c.relname || '|' || a.attname || '|'
         || format_type(a.atttypid, a.atttypmod) || '|' || a.attnotnull::text
         || '|' || coalesce(pg_get_expr(d.adbin, d.adrelid), '')
         || '|' || a.attidentity::text || '|' || a.attgenerated::text
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
    LEFT JOIN pg_attrdef d ON d.adrelid = c.oid AND d.adnum = a.attnum
   WHERE n.nspname = 'public' AND c.relname NOT LIKE 'schema_migrations%'
     AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
  UNION ALL
  SELECT 'idx|' || indexdef FROM pg_indexes
   WHERE schemaname = 'public' AND tablename <> 'schema_migrations'
  UNION ALL
  SELECT 'con|' || c.relname || '|' || k.conname || '|' || pg_get_constraintdef(k.oid)
    FROM pg_constraint k JOIN pg_class c ON c.oid = k.conrelid
    JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE n.nspname = 'public' AND c.relname <> 'schema_migrations'
  UNION ALL
  SELECT 'trg|' || pg_get_triggerdef(t.oid)
    FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid
    JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE n.nspname = 'public' AND NOT t.tgisinternal
     AND c.relname <> 'schema_migrations'
  UNION ALL
  SELECT 'fn|' || pg_get_functiondef(p.oid)
    FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
   WHERE n.nspname = 'public' AND p.prokind IN ('f', 'p')
     AND NOT EXISTS (SELECT 1 FROM pg_depend d
                      WHERE d.objid = p.oid AND d.deptype = 'e')
  UNION ALL
  SELECT 'view|' || c.relname || '|' || pg_get_viewdef(c.oid)
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE n.nspname = 'public' AND c.relkind IN ('v', 'm')
  UNION ALL
  SELECT 'ext|' || extname FROM pg_extension
) s;
"""


def log(name):
    print(f"apply-migration-runtime: passed {name}")


# ---------------------------------------------------------------- plumbing

def _base():
    # Imported lazily: the suite module imports this one, so a top-level
    # import would be circular.
    import test_apply_migration_postgres as base
    return base


def _am():
    import apply_migration
    return apply_migration


def db_env(env, database):
    return {**env, "PGDATABASE": database}


def db_url(env, database):
    user = urllib.parse.quote(env["PGUSER"], safe="")
    password = urllib.parse.quote(env["PGPASSWORD"], safe="")
    return (f"postgresql://{user}:{password}@{env['PGHOST']}:{env['PGPORT']}"
            f"/{database}")


@contextlib.contextmanager
def fresh_database(env, name):
    """Create ``name`` (never IF NOT EXISTS) and drop it afterwards."""
    base = _base()
    base.run_psql(env, f"CREATE DATABASE {name};")
    try:
        yield db_env(env, name), db_url(env, name)
    finally:
        base.run_psql(env, f"DROP DATABASE IF EXISTS {name} WITH (FORCE);")


class Files:
    """Throwaway files with independent, valid app manifests so the
    prior-history guard never couples unrelated cases."""

    def __init__(self, root, files):
        self.root = pathlib.Path(root)
        self.manifests = {}
        for index, (name, body) in enumerate(files.items()):
            data = body if isinstance(body, bytes) else body.encode("utf-8")
            (self.root / name).write_bytes(data)
            manifest = (f'[[migration]]\nfile = "{name}"\ntarget = "app"\n'
                        f'sha256 = "{hashlib.sha256(data).hexdigest()}"\n')
            path = self.root / f"manifest-{index}.toml"
            path.write_text(manifest, encoding="utf-8")
            self.manifests[name] = path
        self.manifest = next(iter(self.manifests.values()))

    def argv(self, name, database, record_only=False, manifest=None,
             applied_by="runtime-fixture"):
        argv = ["--file", str(self.root / name),
                "--manifest", str(manifest or self.manifests[name]),
                "--applied-by", applied_by, "--expect-database", database]
        if record_only:
            argv.append("--record-only")
        return argv


def inproc(url, argv, parent_env=None):
    """Run the real ``main`` in-process; returns (rc, stdout, stderr)."""
    out, err = io.StringIO(), io.StringIO()
    with mock.patch.dict(os.environ, {"DATABASE_URL": url, **(parent_env or {})}):
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = _am().main(argv)
    return rc, out.getvalue(), err.getvalue()


def cli(url, argv, timeout=150):
    child = {k: v for k, v in os.environ.items()
             if k in ("PATH", "HOME", "LANG", "LC_ALL")}
    child["DATABASE_URL"] = url
    return subprocess.run(
        [sys.executable, str(pathlib.Path(_am().__file__)), *argv],
        env=child, text=True, capture_output=True, timeout=timeout)


def bootstrap(url, database):
    """Run step A (tracking DDL) exactly as ``main`` does."""
    am = _am()
    am.psql(am.connection_env(url), am.ddl_sql(database), single_transaction=True)


def exists(env, regclass):
    return _base().scalar(
        env, f"SELECT to_regclass('{regclass}') IS NOT NULL;") == "t"


def receipts(env):
    return _base().scalar(
        env, "SELECT coalesce(string_agg(file || ':' || sha256, ',' ORDER BY file), '') "
             "FROM public.schema_migrations;")


def receipt_rows(env):
    return _base().scalar(
        env, "SELECT coalesce(string_agg(file || ':' || sha256 || ':' || "
             "applied_at::text || ':' || applied_by, ',' ORDER BY file), '') "
             "FROM public.schema_migrations;")


def count(env, table):
    return _base().scalar(env, f"SELECT count(*) FROM {table};")


class PsqlSpy:
    """Run the real ``subprocess.run``; for the two apply invocations
    (``--single-transaction``) prepend a backend probe to the same input
    and record what the live backend reports."""

    def __init__(self, real_run, lose_response_after=None):
        self.real_run = real_run
        self.calls = []
        self.lose_response_after = lose_response_after

    def __call__(self, args, **kwargs):
        if not (args and args[0] == "psql" and "--single-transaction" in args):
            return self.real_run(args, **kwargs)
        original = kwargs["input"]
        probed = {**kwargs, "input": PROBE + original}
        result = self.real_run(args, **probed)
        probe = [line for line in result.stdout.splitlines()
                 if line.startswith("PROBE|")]
        self.calls.append({
            "input": original, "rc": result.returncode, "stderr": result.stderr,
            "probe": probe[0].split("|") if probe else None,
        })
        if (self.lose_response_after and result.returncode == 0
                and self.lose_response_after in original):
            raise subprocess.TimeoutExpired(args, kwargs.get("timeout", 120))
        return result


@contextlib.contextmanager
def spying(lose_response_after=None):
    spy = PsqlSpy(subprocess.run, lose_response_after)
    with mock.patch.object(subprocess, "run", spy):
        yield spy


def assert_controlled(call, label):
    probe = call["probe"]
    assert probe is not None, f"{label}: backend probe produced no output"
    _tag, search_path, timeouts, scs = probe
    assert search_path.replace(" ", "") == CONTROLLED_SEARCH_PATH, \
        f"{label}: backend search_path {search_path!r}"
    assert timeouts == CONTROLLED_TIMEOUTS, f"{label}: backend timeouts {timeouts!r}"
    assert scs == "on", f"{label}: standard_conforming_strings {scs!r}"
    # Audit the SQL sent to this observed child, not just composition or
    # the parent environment. These fixtures have no migration routines.
    sql = call["input"]
    for name in ("current_database", "pg_advisory_xact_lock", "now"):
        assert f"pg_catalog.{name}(" in sql, f"{label}: unqualified {name}"
    if "CREATE TABLE IF NOT EXISTS public.schema_migrations" in sql:
        for name in ("pg_class", "pg_namespace", "pg_attribute",
                     "aclexplode", "format", "pg_get_userbyid"):
            assert f"pg_catalog.{name}" in sql, f"{label}: unqualified {name}"


def catalog_fingerprint(env):
    return _base().scalar(env, FINGERPRINT_SQL)


def mirror_seed(env):
    # updated_at is generated at apply time, not deterministic between
    # independent databases. Compare every other seed column in ID order.
    return _base().scalar(env, """
        SELECT coalesce(jsonb_agg(jsonb_build_array(
            id, last_packet, last_applied_at, lag_seconds) ORDER BY id)::text, '[]')
        FROM public.mb_import_state;""")


def raw_apply(env, path):
    result = subprocess.run(
        list(_base().ARGS), input=pathlib.Path(path).read_text(encoding="utf-8"),
        env={**env, "PGOPTIONS": "-c search_path=public,pg_catalog"},
        text=True, capture_output=True, timeout=120)
    assert result.returncode == 0, f"raw reference apply of {path.name} failed"


# --------------------------------------------------------------- scenarios

def assert_frozen_mirror(manifest):
    # Pin only the ordered mirror entries and their bytes; app migrations
    # may evolve independently of this mirror-only runtime scenario.
    entries = _am().load_manifest(manifest)
    mirror = [e for e in entries if e["target"] == "mirror"]
    # These frozen entries have no rename metadata; compare whole records so
    # additions cannot change accepted aliases or source-history deny-lists.
    expected = [{"file": name, "sha256": sha, "target": "mirror"}
                for name, sha in MIRROR_FILES]
    assert mirror == expected, "repository mirror manifest entries changed"
    for name, sha in MIRROR_FILES:
        data = (manifest.parent / name).read_bytes()
        assert hashlib.sha256(data).hexdigest() == sha, f"{name} bytes changed"


def case_mirror_ordered(env):
    assert_frozen_mirror(REAL_MANIFEST)

    with fresh_database(env, "nntune_rt_mirror_ref") as (ref_env, _ref_url):
        for name, _sha in MIRROR_FILES:
            raw_apply(ref_env, REPO_ROOT / "migrations" / name)
        reference = catalog_fingerprint(ref_env)
        reference_seed = mirror_seed(ref_env)
    assert reference.rsplit(":", 1)[1] != "0", "empty reference catalog"
    assert reference_seed == '[[1, 0, null, null]]', "unexpected reference seed"

    with fresh_database(env, "nntune_rt_mirror") as (menv, url):
        database = "nntune_rt_mirror"
        for index, (name, sha) in enumerate(MIRROR_FILES, start=1):
            proc = cli(url, ["--file", str(REPO_ROOT / "migrations" / name),
                             "--manifest", str(REAL_MANIFEST),
                             "--applied-by", "runtime-mirror",
                             "--expect-database", database])
            assert proc.returncode == 0, f"{name}: apply rc {proc.returncode}"
            assert "recorded for runtime-mirror" in proc.stdout
            expected = ",".join(f"{n}:{s}" for n, s in MIRROR_FILES[:index])
            assert receipts(menv) == expected, f"ledger prefix after {name}"
            assert count(menv, "public.schema_migrations") == str(index)
            assert mirror_seed(menv) == reference_seed, f"seed parity after {name}"
            # Stop/fail at every successful prefix boundary. B's failure
            # must not erase earlier files or leave the attempted effect.
            prefix_catalog = catalog_fingerprint(menv)
            prefix_rows = receipt_rows(menv)
            with tempfile.TemporaryDirectory() as tmp:
                fail = Files(tmp, {"0099_prefix_failure.sql": (
                    "BEGIN; CREATE TABLE prefix_failure (id int); "
                    "SELECT 1 / 0; COMMIT;")})
                rc, _, _ = inproc(
                    url, fail.argv("0099_prefix_failure.sql", database))
            assert rc == 1
            assert not exists(menv, "public.prefix_failure")
            assert receipt_rows(menv) == prefix_rows
            assert catalog_fingerprint(menv) == prefix_catalog
        assert catalog_fingerprint(menv) == reference, \
            "managed catalog differs from raw reference apply"
        before_rows = receipt_rows(menv)
        before_fp = catalog_fingerprint(menv)
        before_seed = _base().scalar(menv,
            "SELECT row_to_json(s)::text FROM public.mb_import_state s ORDER BY id;")
        for name, _sha in MIRROR_FILES:
            proc = cli(url, ["--file", str(REPO_ROOT / "migrations" / name),
                             "--manifest", str(REAL_MANIFEST),
                             "--applied-by", "runtime-mirror-again",
                             "--expect-database", database])
            assert proc.returncode == 0 and "already recorded" in proc.stdout, name
        assert receipt_rows(menv) == before_rows, "re-run changed the ledger"
        assert count(menv, "public.schema_migrations") == str(len(MIRROR_FILES))
        assert catalog_fingerprint(menv) == before_fp == reference
        assert mirror_seed(menv) == reference_seed, "re-run changed deterministic seed"
        assert _base().scalar(menv,
            "SELECT row_to_json(s)::text FROM public.mb_import_state s ORDER BY id;") \
            == before_seed, "re-run changed seed rows (including generated timestamp)"
    log("mirror-ordered-and-idempotent")


def case_concurrent(env):
    base = _base()
    body = ("CREATE TABLE race_effect (id int);\n"
            "SELECT pg_sleep(1.5);\n"
            "INSERT INTO race_effect VALUES (1);\n")
    with fresh_database(env, "nntune_rt_race") as (renv, url), \
            tempfile.TemporaryDirectory() as tmp:
        files = Files(tmp, {"0002_race.sql": body})
        # Start empty: A's bootstrap/ACL and B's effects/receipt must
        # each serialize independently, without a whole-window lock.
        argv = files.argv("0002_race.sql", "nntune_rt_race")
        with ThreadPoolExecutor(max_workers=4) as pool:
            procs = list(pool.map(lambda _i: cli(url, argv), range(4)))
        codes = [p.returncode for p in procs]
        assert codes == [0] * 4, codes
        assert sum("recorded for" in p.stdout for p in procs) == 1
        assert sum("already recorded; nothing applied" in p.stdout
                   for p in procs) == 3
        assert count(renv, "race_effect") == "1"
        assert base.scalar(renv, "SELECT count(*) FROM public.schema_migrations "
                                 "WHERE file = '0002_race.sql';") == "1"
    log("concurrent-same-file-one-effect-one-receipt")


def case_injected_failures(env):
    sql_fail = ("BEGIN;\nCREATE TABLE sqlfail_effect (id int);\n"
                "SELECT 1 / 0;\nCOMMIT;\n")
    trig_body = "CREATE TABLE trig_effect (id int);\n"
    with fresh_database(env, "nntune_rt_inject") as (ienv, url), \
            tempfile.TemporaryDirectory() as tmp:
        database = "nntune_rt_inject"
        files = Files(tmp, {"0002_sqlfail.sql": sql_fail,
                            "0003_trig.sql": trig_body})
        assert not exists(ienv, "public.schema_migrations")

        # Injected SQL failure inside a wrapped file. Transaction A
        # (tracking DDL) committed independently; B left nothing.
        with spying() as spy:
            rc, _out, _err = inproc(url, files.argv("0002_sqlfail.sql", database))
        assert rc == 1
        # The runner redacts SQL stderr: assert on the real psql result
        # code of the B call (A succeeded, B failed), not on text.
        assert [c["rc"] != 0 for c in spy.calls] == [False, True], spy.calls
        assert exists(ienv, "public.schema_migrations"), "transaction A not committed"
        assert count(ienv, "public.schema_migrations") == "0"
        assert not exists(ienv, "public.sqlfail_effect")

        # Injected receipt failure: the migration body ran, the receipt
        # insert is refused by a trigger, the whole B transaction rolls back.
        _base().run_psql(ienv, """
            CREATE FUNCTION public.rt_block_receipt() RETURNS trigger
            LANGUAGE plpgsql AS $f$
            BEGIN RAISE EXCEPTION 'injected receipt failure'; END
            $f$;
            CREATE TRIGGER rt_block BEFORE INSERT ON public.schema_migrations
                FOR EACH ROW EXECUTE FUNCTION public.rt_block_receipt();""")
        with spying() as spy:
            rc, _out, _err = inproc(url, files.argv("0003_trig.sql", database))
        assert rc == 1
        assert [c["rc"] != 0 for c in spy.calls] == [False, True], spy.calls
        assert not exists(ienv, "public.trig_effect"), "effect survived receipt failure"
        assert count(ienv, "public.schema_migrations") == "0"

        # Reconcile: remove the injection, re-run, effect and receipt land once.
        _base().run_psql(ienv, "DROP TRIGGER rt_block ON public.schema_migrations;")
        rc, _out, _err = inproc(url, files.argv("0003_trig.sql", database))
        assert rc == 0
        assert exists(ienv, "public.trig_effect")
        assert count(ienv, "public.schema_migrations") == "1"
    log("injected-failures-roll-back-b-keep-a")


def case_controlled_session(env):
    body = "CREATE TABLE session_probe (id int);\n"
    hostile = {"PGOPTIONS": HOSTILE_PGOPTIONS}
    with fresh_database(env, "nntune_rt_session") as (senv, url), \
            tempfile.TemporaryDirectory() as tmp:
        database = "nntune_rt_session"
        files = Files(tmp, {"0002_probe.sql": body, "0003_probe.sql": body + "-- b\n",
                            "0004_probe.sql": "CREATE TABLE session_probe_cli (id int);\n"})

        # Normal apply (A then B), hostile parent PGOPTIONS.
        with spying() as spy:
            rc, _out, _err = inproc(url, files.argv("0002_probe.sql", database),
                                    parent_env=hostile)
        assert rc == 0 and len(spy.calls) == 2, (rc, len(spy.calls))
        for label, call in zip(("apply-A", "apply-B"), spy.calls):
            assert call["rc"] == 0
            assert_controlled(call, label)
        assert exists(senv, "public.session_probe")

        # Record-only (both paths again).
        with spying() as spy:
            rc, _out, _err = inproc(
                url, files.argv("0003_probe.sql", database, record_only=True),
                parent_env=hostile)
        assert rc == 0 and len(spy.calls) == 2, (rc, len(spy.calls))
        for label, call in zip(("record-A", "record-B"), spy.calls):
            assert_controlled(call, label)
        assert "0003_probe.sql" in receipts(senv)

        # Already-receipt: B refuses under the lock, still controlled.
        with spying() as spy:
            rc, out, _err = inproc(url, files.argv("0002_probe.sql", database),
                                   parent_env=hostile)
        assert rc == 0 and "already recorded" in out
        assert len(spy.calls) == 2 and spy.calls[1]["rc"] != 0
        for label, call in zip(("noop-A", "noop-B"), spy.calls):
            assert_controlled(call, label)

        # Direct psql callers must not bypass the pin with a hostile child
        # env either. Observe both actual backend sessions again.
        am = _am()
        direct = {**am.connection_env(url), "PGOPTIONS": HOSTILE_PGOPTIONS}
        sql, *_ = am.compose(files.root / "0002_probe.sql", files.manifest,
                             "runtime-fixture", expect_database=database)
        with spying() as spy:
            am.psql(direct, am.ddl_sql(database), single_transaction=True)
            try:
                am.psql(direct, sql, single_transaction=True)
            except am._AlreadyRecorded as recorded:
                assert recorded.same
            else:
                raise AssertionError("direct no-op did not find receipt")
        assert len(spy.calls) == 2
        for label, call in zip(("direct-A", "direct-B"), spy.calls):
            assert_controlled(call, label)

        # Same through the CLI process with a hostile parent environment.
        child = {k: v for k, v in os.environ.items()
                 if k in ("PATH", "HOME", "LANG", "LC_ALL")}
        child.update({"DATABASE_URL": url, **hostile})
        proc = subprocess.run(
            [sys.executable, str(pathlib.Path(_am().__file__)),
             *files.argv("0004_probe.sql", database)],
            env=child, text=True, capture_output=True, timeout=150)
        assert proc.returncode == 0, proc.returncode
    log("controlled-session-settings-observed-in-backend")


def case_application_namespace(env):
    """Search-path pin isolates runner from application-schema objects."""
    base = _base()
    database, role = "nntune_rt_namespace", "apply_rt_namespace_reader"
    with fresh_database(env, database) as (nenv, url), \
            tempfile.TemporaryDirectory() as tmp:
        base.run_psql(nenv, """
            CREATE TABLE public.namespace_calls (note pg_catalog.text);
            CREATE FUNCTION public.current_database() RETURNS pg_catalog.name
                LANGUAGE sql AS $$ SELECT 'application_fixture'::pg_catalog.name $$;
            CREATE FUNCTION public.now() RETURNS pg_catalog.timestamptz
                LANGUAGE sql AS $$ SELECT '2000-01-01Z'::pg_catalog.timestamptz $$;
            CREATE FUNCTION public.pg_advisory_xact_lock(pg_catalog.int8) RETURNS void
                LANGUAGE sql AS $$ SELECT pg_catalog.pg_sleep(0) $$;
            CREATE TABLE public.pg_class (
                oid pg_catalog.oid, relnamespace pg_catalog.oid, relname pg_catalog.name,
                relowner pg_catalog.oid, relacl pg_catalog.aclitem[]);
            CREATE TABLE public.pg_namespace (oid pg_catalog.oid, nspname pg_catalog.name);
            CREATE TABLE public.pg_attribute (
                attrelid pg_catalog.oid, attname pg_catalog.name, attnum pg_catalog.int2,
                attisdropped pg_catalog.bool, attacl pg_catalog.aclitem[]);
            CREATE DOMAIN public.text AS pg_catalog.text;
            CREATE DOMAIN public.timestamptz AS pg_catalog.timestamptz;
            CREATE TYPE public.record AS (note pg_catalog.text);
            CREATE FUNCTION public.namespace_equal(pg_catalog.text, pg_catalog.text)
                RETURNS pg_catalog.bool LANGUAGE plpgsql AS $$
                BEGIN
                    INSERT INTO public.namespace_calls VALUES ('equal');
                    RETURN $1 OPERATOR(pg_catalog.=) $2;
                END $$;
            CREATE OPERATOR public.= (
                LEFTARG = pg_catalog.text, RIGHTARG = pg_catalog.text,
                FUNCTION = public.namespace_equal);
        """)
        # Positive fixture control: these objects really are selected by
        # an unqualified application-schema session, not inert test data.
        assert base.scalar(nenv, """
            SET search_path = public,pg_catalog;
            SELECT current_database();""") == "application_fixture"
        assert base.scalar(nenv, """
            BEGIN;
            SET LOCAL search_path = public,pg_catalog;
            SELECT 'a'::pg_catalog.text = 'a'::pg_catalog.text;
            SELECT pg_catalog.count(*) FROM public.namespace_calls;
            ROLLBACK;""") == "t\n1"
        assert count(nenv, "public.namespace_calls") == "0"
        files = Files(tmp, {
            "0002_namespace.sql": (
                "CREATE TABLE public.namespace_effect (held pg_catalog.bool);\n"
                "INSERT INTO public.namespace_effect SELECT EXISTS (\n"
                "  SELECT 1 FROM pg_catalog.pg_locks\n"
                "  WHERE locktype OPERATOR(pg_catalog.=) 'advisory' AND granted\n"
                "    AND objid = 7271901 AND pid = pg_catalog.pg_backend_pid());\n"),
            "0003_namespace.sql": (
                "CREATE TABLE public.namespace_recorded (id pg_catalog.int4);\n"
                "INSERT INTO public.namespace_recorded VALUES (7);\n"),
        })
        hostile = {"PGOPTIONS": HOSTILE_PGOPTIONS}
        with spying() as spy:
            rc, out, _err = inproc(
                url, files.argv("0002_namespace.sql", database), parent_env=hostile)
        assert rc == 0 and "recorded for" in out, "namespace normal apply failed"
        assert len(spy.calls) == 2
        for label, call in zip(("namespace-apply-A", "namespace-apply-B"), spy.calls):
            assert call["rc"] == 0
            assert_controlled(call, label)
        assert base.scalar(nenv, "SELECT held FROM public.namespace_effect;") == "t"
        assert base.scalar(nenv, """
            SELECT pg_catalog.string_agg(a.attname || ':' || a.atttypid::pg_catalog.text,
                                         ',' ORDER BY a.attname)
            FROM pg_catalog.pg_attribute a
            WHERE a.attrelid = 'public.schema_migrations'::pg_catalog.regclass
              AND a.attnum > 0 AND NOT a.attisdropped;""") == (
                  "applied_at:1184,applied_by:25,file:25,sha256:25")
        assert base.scalar(nenv, """
            SELECT pg_catalog.bool_and(applied_at > '2000-01-02Z'::pg_catalog.timestamptz)
            FROM public.schema_migrations;""") == "t"

        # The record-only fixture was independently applied; B must leave
        # its non-idempotent body alone while recording the original hash.
        raw_apply(nenv, files.root / "0003_namespace.sql")
        base.run_psql(env, f"CREATE ROLE {role};")
        try:
            for name, record_only, already in (
                    ("0003_namespace.sql", True, False),
                    ("0002_namespace.sql", False, True),
                    ("0003_namespace.sql", True, True)):
                base.run_psql(nenv, f"""
                    GRANT SELECT, INSERT ON public.schema_migrations TO {role};
                    GRANT SELECT (file), UPDATE (sha256)
                        ON public.schema_migrations TO {role};""")
                before = receipt_rows(nenv)
                with spying() as spy:
                    rc, out, _err = inproc(
                        url, files.argv(name, database, record_only=record_only),
                        parent_env=hostile)
                assert rc == 0, "namespace record/no-op apply failed"
                assert len(spy.calls) == 2 and spy.calls[0]["rc"] == 0
                assert (spy.calls[1]["rc"] != 0) == already
                if already:
                    assert "already recorded" in out
                    assert receipt_rows(nenv) == before
                for label, call in zip(("namespace-record-A", "namespace-record-B"),
                                       spy.calls):
                    assert_controlled(call, label)
                # A must find the actual catalogs and revoke both write
                # grants, even when B takes the existing-receipt path.
                assert base.scalar(nenv, f"""
                    SELECT pg_catalog.has_table_privilege('{role}',
                        'public.schema_migrations', 'SELECT')
                       AND pg_catalog.has_column_privilege('{role}',
                        'public.schema_migrations', 'file', 'SELECT');""") == "t"
                base.assert_psql_fails(nenv, f"""
                    SET ROLE {role};
                    INSERT INTO public.schema_migrations (file, sha256, applied_by)
                    VALUES ('0099_no_write.sql', '{'a' * 64}', 'fixture');""")
                base.assert_psql_fails(nenv, f"""
                    SET ROLE {role};
                    UPDATE public.schema_migrations SET sha256 = '{'b' * 64}';""")
                assert count(nenv, "public.namespace_effect") == "1"
                assert base.scalar(nenv, "SELECT id FROM public.namespace_recorded;") == "7"
                assert count(nenv, "public.schema_migrations") == "2"
                assert count(nenv, "public.namespace_calls") == "0"
            for name in files.manifests:
                sha = hashlib.sha256((files.root / name).read_bytes()).hexdigest()
                assert base.scalar(nenv, f"SELECT sha256 FROM public.schema_migrations "
                                        f"WHERE file = '{name}';") == sha
        finally:
            base.run_psql(nenv, f"DROP OWNED BY {role};")
            base.run_psql(env, f"DROP ROLE {role};")
    log("application-namespace-independent-runner-paths")


def case_wrong_expected_database(env):
    am = _am()
    body = "CREATE TABLE wrongdb_effect (id int);\n"
    other = "nntune_rt_not_this_one"
    with fresh_database(env, "nntune_rt_wrongdb") as (wenv, url), \
            tempfile.TemporaryDirectory() as tmp:
        files = Files(tmp, {"0002_wrongdb.sql": body})
        child = am.connection_env(url)

        # A, bypassing main's URL precheck: nothing created.
        try:
            am.psql(child, am.ddl_sql(other), single_transaction=True)
        except am.ApplyError as exc:
            assert "does not match --expect-database" in str(exc), exc
        else:
            raise AssertionError("wrong-database DDL step was accepted")
        assert not exists(wenv, "public.schema_migrations")

        # B, with the tracking table present: apply and record-only both
        # refuse inside the transaction; no effect, no receipt.
        bootstrap(url, "nntune_rt_wrongdb")
        for record_only in (False, True):
            sql, *_ = am.compose(
                files.root / "0002_wrongdb.sql", files.manifest, "runtime-fixture",
                record_only=record_only, expect_database=other)
            try:
                am.psql(child, sql, single_transaction=True)
            except am.ApplyError as exc:
                assert "does not match --expect-database" in str(exc), exc
            else:
                raise AssertionError("wrong-database guarded apply was accepted")
        assert not exists(wenv, "public.wrongdb_effect")
        assert count(wenv, "public.schema_migrations") == "0"
    log("wrong-expected-database-refused-in-transaction")


def case_timeout_and_unknown_commit(env):
    base = _base()
    timeout_body = ("CREATE TABLE timeout_effect (id int);\n"
                    "SELECT pg_sleep(5);\n")
    lost_body = "CREATE TABLE lost_effect (id int);\n"
    conflict_body = "CREATE TABLE conflict_effect (id int);\n"
    with fresh_database(env, "nntune_rt_recon") as (renv, url), \
            tempfile.TemporaryDirectory() as tmp:
        database = "nntune_rt_recon"
        files = Files(tmp, {"0002_timeout.sql": timeout_body,
                            "0003_lost.sql": lost_body,
                            "0004_conflict.sql": conflict_body})

        # Server-side statement timeout in the migration body: the whole
        # apply rolls back, tracking DDL (A) stays.
        # The child timeout is shortened through the runner's own
        # SESSION_OPTIONS, so the real psql child enforces it.
        am = _am()
        short = am.SESSION_OPTIONS.replace(
            "statement_timeout=90000", "statement_timeout=300")
        assert short != am.SESSION_OPTIONS
        started = time.monotonic()
        with mock.patch.object(am, "SESSION_OPTIONS", short), spying() as spy:
            rc, _out, _err = inproc(url, files.argv("0002_timeout.sql", database))
        assert rc == 1 and time.monotonic() - started < 60
        assert [c["rc"] != 0 for c in spy.calls] == [False, True], spy.calls
        assert spy.calls[1]["probe"][2].split(",")[2] == "300", spy.calls[1]["probe"]
        assert not exists(renv, "public.timeout_effect")
        assert count(renv, "public.schema_migrations") == "0"

        # Unknown commit: B really commits, then the response is lost.
        with spying(lose_response_after="CREATE TABLE lost_effect") as spy:
            rc, _out, err = inproc(url, files.argv("0003_lost.sql", database))
        assert rc == 1, rc
        assert "reconcile" in err and "outcome may be unknown" in err, err
        assert spy.calls and spy.calls[-1]["rc"] == 0, "commit did not happen"
        assert exists(renv, "public.lost_effect")
        assert count(renv, "public.schema_migrations") == "1"
        committed = receipt_rows(renv)
        # Reconcile by re-running: non-idempotent CREATE TABLE must not
        # run again, the receipt stays byte-identical.
        rc, out, _err = inproc(url, files.argv("0003_lost.sql", database))
        assert rc == 0 and "already recorded" in out
        assert receipt_rows(renv) == committed
        assert count(renv, "public.schema_migrations") == "1"

        # Checksum conflict retained while another session holds the
        # advisory lock: the apply waits, then refuses; the conflicting
        # row survives untouched and no effect lands.
        base.run_psql(renv, "INSERT INTO public.schema_migrations "
                            "(file, sha256, applied_by) VALUES "
                            f"('0004_conflict.sql', '{'a' * 64}', 'pre-existing');")
        holder = subprocess.Popen(
            [*base.ARGS, "-c",
             f"SELECT pg_advisory_lock({ADVISORY_KEY}), pg_sleep(3);"],
            env=renv, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            for _ in range(50):
                held = base.scalar(
                    renv, "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' "
                          f"AND granted AND objid = {ADVISORY_KEY};")
                if held != "0":
                    break
                time.sleep(0.1)
            else:
                raise AssertionError("lock holder never acquired the lock")
            started = time.monotonic()
            rc, _out, _err = inproc(url, files.argv("0004_conflict.sql", database))
            waited = time.monotonic() - started
        finally:
            holder.wait(timeout=30)
        assert rc == 1
        assert waited >= 1.0, f"apply did not wait for the held lock ({waited:.2f}s)"
        assert base.scalar(
            renv, "SELECT sha256 || applied_by FROM public.schema_migrations "
                  "WHERE file = '0004_conflict.sql';") == "a" * 64 + "pre-existing"
        assert not exists(renv, "public.conflict_effect")
    log("timeout-unknown-commit-and-conflict-reconciliation")


def case_wrappers(env):
    base = _base()
    safe = {
        "0001_quoted.sql": (
            'BEGIN; CREATE TABLE "COMMIT;" ("BEGIN" text);\n'
            '/* outer /* ROLLBACK; */ nested */\n'
            'INSERT INTO "COMMIT;" VALUES (E\'it\\\'s COMMIT;\'); COMMIT;'),
        "0002_plain.sql": "-- header\nBEGIN;\nCREATE TABLE w_plain (id int);\nCOMMIT;\n",
        "0003_hidden.sql": (
            "-- header mentions COMMIT; and ROLLBACK;\n"
            "BEGIN;\n"
            "CREATE TABLE w_hidden (note text DEFAULT 'COMMIT;');\n"
            "/* ROLLBACK; BEGIN; */\n"
            "INSERT INTO w_hidden VALUES ('it''s COMMIT; here');\n"
            "DO $body$\nBEGIN\n    PERFORM 1;\nEND\n$body$;\n"
            "DO $$ BEGIN PERFORM 'COMMIT;'; END $$;\n"
            "-- COMMIT;\n"
            "COMMIT;\n"
            "-- trailing note\n"),
    }
    rejected = {
        "0010_trailing.sql": "BEGIN;\nCREATE TABLE r_trailing (id int);\nCOMMIT;\n"
                             "CREATE TABLE r_after (id int);\n",
        "0011_two.sql": "BEGIN;\nCREATE TABLE r_two_a (id int);\nCOMMIT;\n"
                        "BEGIN;\nCREATE TABLE r_two_b (id int);\nCOMMIT;\n",
        "0012_rollback.sql": "BEGIN;\nCREATE TABLE r_rb (id int);\nROLLBACK;\n",
        "0013_nested.sql": "BEGIN;\nBEGIN;\nCREATE TABLE r_nested (id int);\n"
                           "COMMIT;\nCOMMIT;\n",
        "0014_nocommit.sql": "BEGIN;\nCREATE TABLE r_nocommit (id int);\n",
        "0015_nobegin.sql": "CREATE TABLE r_nobegin (id int);\nCOMMIT;\n",
        "0016_start.sql": "START TRANSACTION;\nCREATE TABLE r_start (id int);\nCOMMIT;\n",
        "0017_isolation.sql": "BEGIN ISOLATION LEVEL SERIALIZABLE;\n"
                              "CREATE TABLE r_iso (id int);\nCOMMIT;\n",
        "0018_midcommit.sql": "BEGIN;\nCREATE TABLE r_mid_a (id int);\nCOMMIT;\n"
                              "COMMIT;\n",
        "0019_meta.sql": "BEGIN;\nCREATE TABLE r_meta (id int);\n\\echo benign\nCOMMIT;\n",
        "0020_midbegin.sql": "CREATE TABLE r_midbegin (id int);\nBEGIN;\nCOMMIT;\n",
    }
    with fresh_database(env, "nntune_rt_wrap") as (wenv, url), \
            tempfile.TemporaryDirectory() as tmp:
        database = "nntune_rt_wrap"
        files = Files(tmp, {**rejected, **safe})

        # Rejected shapes refuse before any connection: nothing at all.
        for name in rejected:
            rc, _out, _err = inproc(url, files.argv(name, database))
            assert rc == 1, f"{name} was accepted"
        assert not exists(wenv, "public.schema_migrations"), \
            "a rejected wrapper reached the database"
        assert base.scalar(
            wenv, "SELECT count(*) FROM pg_class c JOIN pg_namespace n "
                  "ON n.oid = c.relnamespace WHERE n.nspname = 'public' "
                  "AND c.relname LIKE 'r\\_%';") == "0"

        # Safe wrappers apply once, with the original bytes' checksum.
        for name, body in safe.items():
            rc, out, _err = inproc(url, files.argv(name, database))
            assert rc == 0 and "recorded for" in out, name
            sha = hashlib.sha256(body.encode()).hexdigest()
            assert base.scalar(
                wenv, f"SELECT sha256 FROM public.schema_migrations "
                      f"WHERE file = '{name}';") == sha
        assert exists(wenv, "public.w_plain") and exists(wenv, "public.w_hidden")
        assert count(wenv, "public.w_hidden") == "1"
        assert base.scalar(wenv, "SELECT note FROM public.w_hidden;") \
            == "it's COMMIT; here"
        assert count(wenv, "public.schema_migrations") == str(len(safe))

        # --record-only is not a workaround: rejected shapes refuse there too.
        for name in rejected:
            rc, _out, _err = inproc(
                url, files.argv(name, database, record_only=True))
            assert rc == 1, f"{name} was accepted by --record-only"
        assert count(wenv, "public.schema_migrations") == str(len(safe))
    log("outer-transaction-wrapper-cases")


def case_noop_acl(env):
    base = _base()
    role = "apply_rt_acl_role"
    with fresh_database(env, "nntune_rt_acl") as (aenv, url), \
            tempfile.TemporaryDirectory() as tmp:
        database = "nntune_rt_acl"
        files = Files(tmp, {"0002_acl.sql": "CREATE TABLE acl_effect (id int);\n"})
        argv = files.argv("0002_acl.sql", database)
        rc, _out, _err = inproc(url, argv)
        assert rc == 0
        base.run_psql(env, f"CREATE ROLE {role};")
        try:
            base.run_psql(aenv, f"""
                GRANT SELECT, INSERT, TRIGGER ON public.schema_migrations TO {role};
                GRANT SELECT (file), INSERT (applied_by), UPDATE (sha256)
                    ON public.schema_migrations TO {role};""")
            before = receipt_rows(aenv)
            rc, out, _err = inproc(url, argv)
            assert rc == 0 and "already recorded" in out
            assert receipt_rows(aenv) == before
            table_privs = base.scalar(aenv, f"""
                SELECT string_agg(a.privilege_type, ',' ORDER BY a.privilege_type)
                FROM pg_class c CROSS JOIN aclexplode(c.relacl) a
                WHERE c.oid = to_regclass('public.schema_migrations')
                  AND a.grantee = '{role}'::regrole;""")
            assert table_privs == "SELECT", table_privs
            column_privs = base.scalar(aenv, f"""
                SELECT string_agg(DISTINCT a.privilege_type, ',')
                FROM pg_attribute t CROSS JOIN aclexplode(t.attacl) a
                WHERE t.attrelid = to_regclass('public.schema_migrations')
                  AND a.grantee = '{role}'::regrole;""")
            assert column_privs == "SELECT", column_privs
        finally:
            with contextlib.suppress(RuntimeError):
                base.run_psql(aenv, f"DROP OWNED BY {role};")
            base.run_psql(env, f"DROP ROLE IF EXISTS {role};")
    log("noop-rerun-pins-write-grants-keeps-reads")


def run(env, test_env):
    """Entry point; ``test_env`` (the suite's database) is not reused."""
    assert test_env.get("PGDATABASE") != env.get("PGDATABASE")
    for case in (case_mirror_ordered, case_concurrent, case_injected_failures,
                 case_controlled_session, case_application_namespace,
                 case_wrong_expected_database,
                 case_timeout_and_unknown_commit, case_wrappers, case_noop_acl):
        case(env)


if __name__ == "__main__":
    raise SystemExit("run via test_apply_migration_postgres.py")
