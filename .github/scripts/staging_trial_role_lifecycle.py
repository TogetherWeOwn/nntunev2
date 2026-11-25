"""Manual, default-off credential bootstrap, grant, readback and retirement."""

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys

from apply_migration import ADVISORY_LOCK_KEY, ApplyError
from staging_apply import StagingApplyError, evidence_child_env, load_entries, quote_literal
from staging_trial_role_acl import GUARD, LOCK_TABLES, WINNERS, WRITER, audit, change, grants, identities, require
from staging_trial_role_preflight import BASELINE, DATABASE, LOGIN_RE, SCOPE_ENV, oid, parse_scope
from staging_trial_schema import read_source

TARGET = "planetscale:nntunev2/main/nntunev2_staging@16500"
NAMES = ("nntune_trial_reader", "nntune_trial_writer")
ROOT = Path(__file__).resolve().parents[2]
FIXTURE = "nntune-private-trial-synthetic-fixture"


def connection(key, route, fixture):
    env = evidence_child_env(os.environ[f"PRIVATE_TRIAL_{key}_URL"], DATABASE)
    host = "127.0.0.1" if fixture else os.environ[f"PRIVATE_TRIAL_{route}_HOST"]
    if (env["PGHOST"] != host or not env["PGPASSWORD"] or not LOGIN_RE.fullmatch(env["PGUSER"])
            or not fixture and not re.fullmatch(r"[a-z0-9.-]+\.psdb\.cloud", host)):
        raise ValueError
    env.update(PGGSSENCMODE="disable", PGPASSFILE="/dev/null")
    if fixture:
        env.update(PGSSLMODE="disable")
    return env


def psql(env, sql, stage):
    proc = subprocess.run(
        ["psql", "-XqAtw", "-v", "ON_ERROR_STOP=1"], input=sql,
        capture_output=True, text=True, env=env, timeout=60,
    )
    if proc.returncode:
        raise StagingApplyError(stage)
    print(f"{stage} passed")


def target(scope, login, fixture):
    observer = login.partition(".")[0]
    marker = f"AND shobj_description(oid, 'pg_database') = '{FIXTURE}'" if fixture else ""
    return require(f"""
session_user = '{observer}' AND current_user = session_user AND current_database() = '{DATABASE}'
AND EXISTS (SELECT FROM pg_database WHERE datname = current_database()
            AND oid = {scope['database_oid']} {marker})
""")


def baseline():
    digest, = (e["sha256"] for e in load_entries(ROOT) if e["file"] == BASELINE and e["target"] == "app")
    return require(f"""SELECT count(*) = 1 AND bool_and(sha256 = '{digest}')
                       FROM public.schema_migrations WHERE file = '{BASELINE}'""")


def lock_contract(scope, action):
    writer = scope["writer"][1]
    src = read_source(ROOT, "scripts/private_trial_publication_locks.sql").decode()
    body = src[src.index("DO $contract$"):src.rindex("COMMIT;")]
    tables = ", ".join(f"'public.{table}'::regclass" for table in LOCK_TABLES)
    enabled = f"""SELECT count(*) = 5 AND bool_and(tgrelid IN ({tables}) AND tgenabled = 'A'
                   AND tgtype = 18 AND tgfoid = to_regprocedure('{GUARD}')
                   AND pg_get_expr(tgqual,tgrelid) IS NOT DISTINCT FROM format('(CURRENT_USER = pg_get_userbyid(%L::oid))', '{writer}'))
                  FROM pg_trigger WHERE tgrelid IN ({tables}) AND tgname = 'private_trial_lock_only'"""
    guard_body = quote_literal(src.split("$guard$")[1])
    guard = require(f"""SELECT NOT prosecdef AND prosrc = {guard_body}
                        AND proconfig = ARRAY['search_path=pg_catalog']
                        AND obj_description(oid,'pg_proc') = '{writer}'
                        FROM pg_proc WHERE oid = to_regprocedure('{GUARD}')""")
    clean = require(f"""NOT EXISTS (SELECT FROM pg_trigger WHERE tgrelid IN ({tables})
                                  AND tgname = 'private_trial_lock_only')
                       AND NOT EXISTS (SELECT FROM pg_class c CROSS JOIN pg_roles r
                         WHERE r.oid = {writer} AND c.oid IN ({tables})
                           AND has_any_column_privilege(r.oid, c.oid, 'UPDATE'))""")
    sql = f"""SELECT to_regprocedure('{GUARD}') IS NOT NULL AS installed \\gset
\\if :installed
{guard}
{require(enabled)}
\\else
{clean}
\\endif
"""
    if action in ("check", "verify"):
        return sql + (require(f"to_regprocedure('{GUARD}') IS NOT NULL") if action == "verify" else "")
    setup = (f"SET LOCAL nntune.private_trial_writer_oid = '{writer}';\n"
             f"SET LOCAL nntune.private_trial_lock_action = '{action}';\n")
    if action == "grant":
        return sql + f"\\if :installed\n\\else\n{setup}{body}\\endif\n" + require(enabled)
    return sql + f"\\if :installed\n{setup}{body}\\endif\n"


def run(action, scope, fixture):
    admin = connection("OWNER", "DIRECT", fixture) if action in ("grant", "fence", "retire") else None
    app = connection("EXECUTOR", "DIRECT", fixture) if action != "fence" else None
    if ((admin and not fixture and admin["PGUSER"].partition(".")[0] != "postgres")
            or (app and app["PGUSER"].partition(".")[0] in NAMES)):
        raise ValueError
    base = list(grants(DATABASE, scope))
    connect = [g for g in base if g[2] == "DATABASE"]
    app_grants = [g for g in base if g[2] != "DATABASE"]
    full = list(grants(DATABASE, scope, locks=True, overlay=(action == "verify-overlay")))
    ids = ", ".join(str(scope[k][1]) for k in ("reader", "writer"))
    drained = require(f"NOT EXISTS (SELECT FROM pg_stat_activity WHERE usesysid IN ({ids}))")
    names = ", ".join(map(quote_literal, NAMES))
    select_names = f"SELECT rolname FROM pg_roles WHERE oid IN ({ids}) AND rolname IN ({names})"
    def commands(statement):
        return f"SELECT format('{statement}', rolname) FROM ({select_names}) r \\gexec\n"

    def revoke(entries):
        return "".join(f"SELECT {quote_literal(change([g], 'REVOKE'))} WHERE EXISTS "
                       f"(SELECT FROM pg_roles WHERE oid = {g[1]}) \\gexec\n" for g in entries)

    def stage(env, label, sql, *, readonly=False):
        mode = "READ ONLY" if readonly else "READ WRITE"
        owner_check = require("SELECT datdba = to_regrole(current_user)::oid FROM pg_database "
                              "WHERE datname = current_database()") if env is admin else ""
        psql(env, f"BEGIN {mode};\n" + target(scope, env["PGUSER"], fixture) + owner_check
             + f"SELECT pg_advisory_xact_lock({ADVISORY_LOCK_KEY});\n"
             + (baseline() if env is app else "") + sql + "\nCOMMIT;\n", label)

    if action in ("fence", "retire"):
        absent_ok = identities(scope, missing=True, login=False)
        fenced = require(f"NOT EXISTS ({select_names} AND rolcanlogin)")
        stage(admin, "login fence", absent_ok + commands("ALTER ROLE %I NOLOGIN") + fenced)
        absent_ok += fenced
        stage(admin, "staging drain", absent_ok + require(
            f"SELECT COALESCE(bool_and(pg_terminate_backend(pid, 5000)),true) FROM pg_stat_activity "
            f"WHERE datname = current_database() AND usesysid IN ({ids})") + "\nSELECT pg_stat_clear_snapshot();\n" + drained)
        if action == "retire":
            stage(app, "application withdrawal", absent_ok + drained + audit(full, exact=False)
                  + lock_contract(scope, "revoke") + revoke(app_grants) + audit(connect, exact=False))
            stage(admin, "role retirement", absent_ok + drained + audit(connect, exact=False) + revoke(connect)
                  + commands("DROP ROLE %I") + require(f"NOT EXISTS (SELECT FROM pg_roles WHERE oid IN ({ids}) "
                  f"OR rolname IN ({names}))") + drained)
        return

    cols = ", ".join(map(quote_literal, WRITER[WINNERS.removeprefix("public.")].split()))
    source = read_source(ROOT, "migrations/0043_storage_object_intents.sql").decode()
    immutable = quote_literal(source.split("FUNCTION public.storage_object_intent_append_only()", 1)[1].split("$$")[1])
    winner = require(f"""
ARRAY(SELECT attname::text FROM pg_attribute WHERE attrelid = '{WINNERS}'::regclass
      AND attnum > 0 AND NOT attisdropped ORDER BY attname::text)
  = ARRAY(SELECT unnest(ARRAY[{cols}]) ORDER BY 1)
AND EXISTS (SELECT FROM pg_trigger t JOIN pg_proc p ON p.oid = t.tgfoid
  WHERE tgrelid = '{WINNERS}'::regclass AND tgname = 'storage_validated_digest_winner_immutable'
    AND tgenabled IN ('O','A') AND tgtype = 58 AND tgqual IS NULL AND NOT p.prosecdef
    AND p.oid = 'public.storage_object_intent_append_only()'::regprocedure AND p.prosrc = {immutable}
    AND (SELECT attnum FROM pg_attribute WHERE attrelid = t.tgrelid AND attname = 'object_id') = ANY(t.tgattr))
""")
    identity_check = identities(scope) + winner
    routed = [connection(key.upper(), "ROUTED", fixture) for key in ("reader", "writer")]
    for name, env in zip(NAMES, routed, strict=True):
        if env["PGUSER"].partition(".")[0] != name:
            raise ValueError
    if action == "grant":
        stage(app, "staging baseline", "", readonly=True)
        stage(admin, "staging CONNECT", identity_check + lock_contract(scope, "check") + audit(full, exact=False)
              + change(connect, "GRANT") + identities(scope))
        stage(app, "application grants", identity_check + audit(full, exact=False) + change(app_grants, "GRANT")
              + lock_contract(scope, "grant") + audit(full, exact=True) + identities(scope))
    stage(app, "exact application ACL", identity_check + lock_contract(scope, "verify")
          + audit(full, exact=True), readonly=True)
    for key, env in zip(("reader", "writer"), routed, strict=True):
        name, role_oid = scope[key]
        psql(env, "BEGIN READ ONLY;\n" + target(scope, env["PGUSER"], fixture)
             + require(f"session_user = '{name}' AND to_regrole(session_user)::oid = {role_oid}") + "COMMIT;\n",
             f"routed {key} {env['PGUSER']} {name} {role_oid}")


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("action", choices=("grant", "verify", "verify-overlay", "fence", "retire"))
    pins = cli.add_mutually_exclusive_group(required=True)
    pins.add_argument("--target", choices=[TARGET])
    pins.add_argument("--fixture-only-oid", type=int)
    args = cli.parse_args()
    fixture = args.fixture_only_oid is not None
    try:
        scope = parse_scope(os.environ[SCOPE_ENV])
        if (tuple(scope[k][0] for k in ("reader", "writer")) != NAMES
                or scope["database_oid"] != (args.fixture_only_oid if fixture else 16500)
                or fixture and (not oid(args.fixture_only_oid) or args.fixture_only_oid == 16500)):
            raise ValueError
        run(args.action, scope, fixture)
    except StagingApplyError as exc:
        # The URL helper also emits only fixed errors. Never relay psql output.
        print(f"Trial role lifecycle refused: {exc}", file=sys.stderr)
        return 1
    except (ApplyError, KeyError, ValueError, OSError, subprocess.TimeoutExpired):
        print("Trial role lifecycle refused", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
