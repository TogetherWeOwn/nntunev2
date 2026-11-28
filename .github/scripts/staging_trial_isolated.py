"""Target admission and overlay ownership for the isolated staging trial."""

import json
import re

from apply_migration import transaction_body
from staging_apply import quote_literal as literal
from staging_stream_config import TRIAL_DATABASES
from staging_trial_role_acl import LOCK_TABLES, ROUTINES, require
from staging_trial_role_preflight import DATABASE, LOGIN_RE, oid
from staging_trial_schema import LOCATOR_CHECK, TABLES, compose, read_source

CONFIG_ENV = "ISOLATED_TRIAL_TARGET"
SHARED_OID = 16500
ROUTES = {"OWNER": "direct", "EXECUTOR": "direct", "READER": "routed", "WRITER": "routed"}

KIND = "selfhost-pg18"
HOSTS = {"direct": "nntune-trial-db-op.nntune.com", "routed": "nntune-trial-db-hd.nntune.com"}
FIXTURE_HOSTS = {"direct": "op.trial.invalid", "routed": "routed.trial.invalid"}
NAMES = ("nntune_trial_reader", "nntune_trial_writer")  # PlanetScale lifecycle
SERVING = tuple(role.lower() for role in TRIAL_DATABASES)
ROLES = {role: f"nntune_trial_{role}" for role in ("owner", "executor", *SERVING)}


def target_config(raw, scope, fixture):
    if len(raw) > 2048:
        raise ValueError
    config = json.loads(raw)
    keys = {"database_oid", "executor_oid", "direct", "direct_port", "routed", "routed_port",
            *(key.lower() for key in ROUTES)}
    if (not isinstance(config, dict) or config.keys() != keys
            or not oid(config["database_oid"])
            or (scope is not None and config["database_oid"] != scope["database_oid"])
            or config["database_oid"] == SHARED_OID or not oid(config["executor_oid"])):
        raise ValueError
    for route in ("direct", "routed"):
        host = config[route]
        port = config[f"{route}_port"]
        if type(port) is not int or not 1 <= port <= 65535:
            raise ValueError
        if not isinstance(host, str) or not (host == "127.0.0.1" if fixture else
                re.fullmatch(r"[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)*\.psdb\.cloud", host)):
            raise ValueError
    for key in ROUTES:
        login = config[key.lower()]
        if not isinstance(login, str) or not LOGIN_RE.fullmatch(login):
            raise ValueError
    if (not fixture and config["owner"].partition(".")[0] != "postgres"
            or (scope is not None and any(config[key].partition(".")[0] != scope[key][0]
                                          for key in ("reader", "writer")))):
        raise ValueError
    return config


def selfhost_target_config(raw, *, fixture=False):
    """Read separately supplied evidence pins. The PlanetScale parser is separate."""
    if len(raw) > 4096:
        raise ValueError
    pins = json.loads(raw)
    if (not isinstance(pins, dict) or pins.keys() != {
            "kind", "system_identifier", "database_oid", "ca_sha256", "server_sha256", "routes", "role_oids"}
            or pins["kind"] != KIND or not oid(pins["database_oid"]) or pins["database_oid"] in (16494, SHARED_OID)
            or type(pins["system_identifier"]) is not int or not 0 < pins["system_identifier"] < 2**64
            or any(not isinstance(pins[key], str) or not re.fullmatch(r"[a-f0-9]{64}", pins[key])
                   for key in ("ca_sha256", "server_sha256"))):
        raise ValueError
    hosts = FIXTURE_HOSTS if fixture else HOSTS
    routes, roles = pins["routes"], pins["role_oids"]
    if (not isinstance(routes, dict) or routes.keys() != hosts.keys()
            or any(not isinstance(routes[key], list) or len(routes[key]) != 2 or routes[key][0] != host
                   or type(routes[key][1]) is not int or not 1 <= routes[key][1] <= 65535
                   for key, host in hosts.items())
            or not isinstance(roles, dict) or roles.keys() != ROLES.keys()
            or not all(oid(roles[key]) for key in ("owner", "executor"))
            or any(roles[key] is not None and not oid(roles[key]) for key in SERVING)
            or len({value for value in roles.values() if value is not None}) != sum(
                value is not None for value in roles.values())):
        raise ValueError
    return pins


def authority(root, app_grants):
    tables = ", ".join(f"'public.{table}'::regclass" for table in LOCK_TABLES)
    sql = require(f"""SELECT count(*) = {len(LOCK_TABLES)} AND bool_and(relowner = to_regrole(current_user)::oid)
                       FROM pg_class WHERE oid IN ({tables})""")
    sql += require("has_schema_privilege('public', 'CREATE')")
    sql += require("to_regclass('public.storage_s3_locator_intents') IS NOT NULL OR "
                   "has_function_privilege('public.storage_object_intent_append_only()', 'EXECUTE')")
    checks = []
    for _, _, kind, obj, privilege, columns in app_grants:
        if columns:
            checks.extend(f"has_column_privilege('{obj}', '{column}', '{privilege} WITH GRANT OPTION')"
                          for column in columns.split())
        else:
            checks.append(f"has_{kind.lower()}_privilege('{obj}', '{privilege} WITH GRANT OPTION')")
    checks.extend(f"has_column_privilege('public.{table}', '{column}', 'REFERENCES')" for table, column in (
        ("storage_object_envelope_intents", "object_id"), ("storage_object_envelope_intents", "root_version_id"),
        ("tracks", "id"), ("ingest_jobs", "id"), ("storage_validated_digest_winners", "object_id"),
        ("invite_codes", "id"), ("users", "id"),
    ))
    return sql + require(" AND ".join(dict.fromkeys(checks))) + routines(root)


def routines(root):
    source = read_source(root, "migrations/0052_storage_digest_reference_admission.sql").decode()
    sql = ""
    for signature, language, result in zip(ROUTINES, ("sql", "plpgsql"),
                                          ("public.storage_validated_digest_winners", "boolean"), strict=True):
        name = signature.partition("(")[0]
        body = source.split(f"FUNCTION {name}(", 1)[1].split("$$")[1]
        sql += require(f"""SELECT NOT prosecdef AND prokind = 'f' AND provolatile = 'v' AND NOT proisstrict
                            AND proconfig = ARRAY['search_path=pg_catalog, public'] AND prosrc = {literal(body)}
                            AND prorettype = '{result}'::regtype AND NOT proretset AND l.lanname = '{language}'
                           FROM pg_proc p JOIN pg_language l ON l.oid = p.prolang
                           WHERE p.oid = '{signature}'::regprocedure""")
    return sql


def overlay(root, scope, executor_oid, action, *, installed_sql="", absent_sql=""):
    reader, reader_oid = scope["reader"]
    writer, writer_oid = scope["writer"]
    # Bind retries and revocation to the actual logins, even after reissuance
    # of the same SQL names. Withdrawal still works after the recipe changes
    # on main because it revokes the audited grants without applying DDL.
    if action == "retire":
        receipt_match = f"~ '^isolated-private-trial [0-9a-f]{{64}} {reader_oid} {writer_oid}$'"
    else:
        sql, digest = compose(root, root, writer, reader, database=DATABASE)
        receipt = f"isolated-private-trial {digest} {reader_oid} {writer_oid}"
        receipt_match = f"= '{receipt}'"
    tables = ", ".join(f"to_regclass('{table}')" for table in TABLES.split(", "))
    admission = "public.private_library_admissions"
    count = f"num_nonnulls({tables}, to_regprocedure('{LOCATOR_CHECK}'))"
    owned = require(f"""{count} = 4
        AND obj_description('{admission}'::regclass, 'pg_class') {receipt_match}
        AND (SELECT bool_and(relowner = {executor_oid}) FROM pg_class WHERE oid IN ({tables}))
        AND (SELECT proowner = {executor_oid} AND NOT prosecdef
             FROM pg_proc WHERE oid = '{LOCATOR_CHECK}'::regprocedure)""")
    if action == "verify-overlay":
        return owned
    present = f"SELECT {count} <> 0 AS overlay_present \\gset\n\\if :overlay_present\n"
    if action in ("check", "retire"):
        return present + owned + installed_sql + "\n\\else\n" + absent_sql + "\n\\endif\n"
    return (present + owned + "\\else\n" + transaction_body(sql)
            + f"SET LOCAL search_path = pg_catalog;\nCOMMENT ON TABLE {admission} IS '{receipt}';\n\\endif\n")
