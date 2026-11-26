"""Qualify staging trial logins and evaluate scoped serving identities."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys
from typing import NamedTuple

from staging_api_identity import SHA, UUID, valid
from staging_apply import SECRET_ENV, StagingApplyError, evidence_child_env, load_entries
from staging_stream_config import TRIAL_DATABASES
from staging_trial_schema import BASELINE, ROLE_RE


DATABASE = "nntunev2_staging"
SCOPE_ENV = "PRIVATE_TRIAL_ROLE_PREFLIGHT_SCOPE"
LOGIN_RE = re.compile(r"[A-Za-z0-9_-]{1,63}(?:\.[A-Za-z0-9_-]{1,63})?")
BINDINGS = tuple(f"PRIVATE_TRIAL_{role}_DB" for role in TRIAL_DATABASES)
BINDING_IDENTITY_SQL = """
SELECT d.datname::text, d.oid, session_user::text, login_role.oid, current_user::text, effective_role.oid
FROM pg_catalog.pg_database d
JOIN pg_catalog.pg_roles login_role ON login_role.rolname = session_user
JOIN pg_catalog.pg_roles effective_role ON effective_role.rolname = current_user
WHERE d.datname = pg_catalog.current_database();
"""


def oid(value, *, optional=False):
    return type(value) is int and (0 if optional else 1) <= value < 2**32


def _valid_binding_role_pins(role_pins):
    if type(role_pins) is not dict or role_pins.keys() != set(BINDINGS):
        return False
    pins = role_pins.values()
    if any(type(pin) is not tuple or len(pin) != 2 or type(pin[0]) is not str
           or not ROLE_RE.fullmatch(pin[0]) or pin[0].lower() == "public" or not oid(pin[1])
           for pin in pins):
        return False
    return (len({name for name, _ in pins}) == len(BINDINGS)
            and len({role_oid for _, role_oid in pins}) == len(BINDINGS))


def binding_identity_verdicts(database_oid, role_pins, rows):
    """Compare one read-only result per binding against independently reviewed pins.

    The database OID comes from target admission. Each binding's pin is its
    configured _ROLE name and reviewed OID. Supply a row only when the same
    binding returned exactly one row. All four bindings must match to satisfy
    the role identity check. Origin, TLS, cache and serving version evidence
    are separate.
    """
    verdicts = {binding: "pending" for binding in BINDINGS}
    if (not oid(database_oid) or not _valid_binding_role_pins(role_pins)
            or type(rows) is not dict or not rows.keys() <= verdicts.keys()):
        return verdicts

    for binding in BINDINGS:
        row = rows.get(binding)
        name, role_oid = role_pins[binding]
        if (type(row) is tuple and len(row) == 6
                and all(type(row[i]) is str and oid(row[i + 1]) for i in (0, 2, 4))
                and row == (DATABASE, database_oid, name, role_oid, name, role_oid)):
            verdicts[binding] = "matched"
    return verdicts


class BindingReadback(NamedTuple):
    build_sha: str
    version_id: str
    binding: str
    rows: tuple[tuple, ...]


def read_binding_identities(*, build_sha, version_id, database_oid, role_pins, query=None):
    """Return per-binding SQL identity evidence for a pinned Worker candidate.

    The optional query takes (build_sha, version_id, binding, sql). It must
    execute the fixed SELECT on that candidate's named connection and return
    a BindingReadback with independently observed build, version and binding.
    The caller supplies the admitted candidate and role pins and separately
    verifies config scope, origin TLS and caching.
    """
    pins = role_pins.copy() if _valid_binding_role_pins(role_pins) else {}
    rows = {}
    if (query is not None and pins and oid(database_oid)
            and valid(SHA, build_sha) and valid(UUID, version_id)):
        for binding in BINDINGS:
            try:
                result = query(build_sha, version_id, binding, BINDING_IDENTITY_SQL)
            except Exception:
                # Query failures may contain connection details.
                continue
            if type(result) is not BindingReadback:
                continue
            observed = (result.build_sha, result.version_id, result.binding)
            if (all(type(value) is str for value in observed)
                    and observed == (build_sha, version_id, binding)
                    and type(result.rows) is tuple and len(result.rows) == 1):
                rows[binding] = result.rows[0]
    verdicts = binding_identity_verdicts(database_oid, pins, rows)
    return {
        binding: {"role": pins[binding][0] if pins else None, "database": DATABASE,
                  "verdict": "pass" if verdicts[binding] == "matched" else "pending"}
        for binding in BINDINGS
    }


def parse_scope(raw):
    if len(raw) > 1024:
        raise ValueError
    scope = json.loads(raw)
    if not isinstance(scope, dict) or scope.keys() != {"database_oid", "issuer_oid", "reader", "writer"}:
        raise ValueError
    reader, writer = scope["reader"], scope["writer"]
    if (not oid(scope["database_oid"]) or not oid(scope["issuer_oid"], optional=True)
            or any(not isinstance(role, list) or len(role) != 2
                   or not isinstance(role[0], str) or not ROLE_RE.fullmatch(role[0])
                   or role[0].lower() == "public" or not oid(role[1]) for role in (reader, writer))
            or reader[0] == writer[0] or len({scope["issuer_oid"], reader[1], writer[1]}) != 3):
        raise ValueError
    return scope


def observe(child, scope, digest):
    # PlanetScale can append a provider routing suffix to the libpq login.
    login = child["PGUSER"]
    if not LOGIN_RE.fullmatch(login) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise ValueError
    observer = login.partition(".")[0]
    if observer in (scope["reader"][0], scope["writer"][0]):
        raise ValueError
    reader, writer = scope["reader"], scope["writer"]
    # All interpolated values are validated identifiers, integers or digests.
    # Check identity before accessing baseline or privilege catalogs.
    sql = rf"""BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;
SET LOCAL search_path = pg_catalog;
SELECT current_database() = '{DATABASE}'
   AND session_user = '{observer}' AND current_user = session_user
   AND EXISTS (SELECT FROM pg_database WHERE datname = current_database()
               AND oid = {scope['database_oid']}) AS correct_target
\gset
\if :correct_target
SELECT count(*) = 1 AND COALESCE(bool_and(sha256 = '{digest}'), false) AS correct_baseline
  FROM public.schema_migrations WHERE file = '{BASELINE}'
\gset
\if :correct_baseline
WITH requested(rolname, oid) AS (VALUES ('{reader[0]}', {reader[1]}::oid), ('{writer[0]}', {writer[1]}::oid)),
roles AS (SELECT r.* FROM pg_roles r JOIN requested q ON q.rolname = r.rolname AND q.oid = r.oid),
app_schemas AS (SELECT oid FROM pg_namespace WHERE nspname NOT LIKE 'pg\_%' AND nspname <> 'information_schema')
SELECT count(*) = 2,
       COALESCE(bool_and(rolcanlogin AND NOT (rolsuper OR rolcreaterole OR rolcreatedb OR
           rolinherit OR rolreplication OR rolbypassrls)), false),
       NOT EXISTS (SELECT FROM pg_auth_members m
          WHERE m.member IN (SELECT oid FROM roles)
             OR (m.roleid IN (SELECT oid FROM roles) AND NOT (
                  m.member = {scope['issuer_oid']} AND m.admin_option
                  AND NOT (m.inherit_option OR m.set_option)
                  AND EXISTS (SELECT FROM pg_roles i WHERE i.oid = m.member AND i.rolcreaterole)))),
       COALESCE(bool_and(has_database_privilege(roles.oid, current_database(), 'CONNECT')
           AND NOT has_database_privilege(roles.oid, current_database(), 'CREATE,TEMPORARY')
           AND NOT has_database_privilege(roles.oid, current_database(), 'CONNECT WITH GRANT OPTION')
           AND has_schema_privilege(roles.oid, 'public', 'USAGE')
           AND NOT has_schema_privilege(roles.oid, 'public', 'USAGE WITH GRANT OPTION')
           AND EXISTS (SELECT FROM pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) a
                WHERE d.datname = current_database() AND a.grantee = roles.oid AND a.privilege_type = 'CONNECT')), false),
       NOT EXISTS (SELECT FROM roles CROSS JOIN pg_database d
          WHERE d.datallowconn AND NOT d.datistemplate AND d.datname <> current_database()
            AND has_database_privilege(roles.oid, d.oid, 'CONNECT')),
       NOT EXISTS (SELECT FROM pg_shdepend d JOIN roles ON roles.oid = d.refobjid
          WHERE d.refclassid = 'pg_authid'::regclass AND d.deptype = 'o'),
       NOT EXISTS (SELECT FROM roles CROSS JOIN app_schemas s
          WHERE has_schema_privilege(roles.oid, s.oid, 'CREATE')),
       NOT EXISTS (SELECT FROM roles CROSS JOIN pg_class c JOIN app_schemas s ON s.oid = c.relnamespace
          WHERE CASE
              WHEN c.relkind = 'S' THEN has_sequence_privilege(roles.oid, c.oid, 'USAGE,SELECT,UPDATE')
              WHEN c.relkind IN ('r', 'p', 'v', 'm', 'f') THEN
                  has_any_column_privilege(roles.oid, c.oid, 'SELECT,INSERT,UPDATE,REFERENCES') OR
                  has_table_privilege(roles.oid, c.oid, 'DELETE,TRUNCATE,TRIGGER,MAINTAIN')
              ELSE false END),
       NOT EXISTS (SELECT FROM roles CROSS JOIN pg_proc p JOIN app_schemas s ON s.oid = p.pronamespace
          WHERE p.prosecdef AND has_schema_privilege(roles.oid, s.oid, 'USAGE')
            AND has_function_privilege(roles.oid, p.oid, 'EXECUTE'))
  FROM roles;
\endif
\endif
COMMIT;
"""
    result = subprocess.run(
        ["psql", "--no-psqlrc", "--no-password", "--quiet", "--tuples-only", "--no-align",
         "--set", "ON_ERROR_STOP=1"], input=sql, text=True, capture_output=True, timeout=60, env=child)
    if result.returncode or not re.fullmatch(r"[tf](?:\|[tf]){8}\n", result.stdout):
        raise StagingApplyError("staging target, baseline or catalog read could not be verified")
    checks = ("issued identities", "login attributes", "role membership", "staging access", "database isolation",
              "object ownership", "schema creation", "relation and sequence access", "privileged routines")
    return dict(zip(checks, (value == "t" for value in result.stdout.strip().split("|")), strict=True))


def binding_connection_verdict(identity_verdicts, origin_tls_attestations):
    """Evaluate the four-binding readback from independently verified verdicts.

    A TLS verdict is True only after the existing origin TLS evidence has
    been verified for that binding's connection. This check does not collect
    evidence or determine whether the trial can be armed.
    """
    bindings = {f"PRIVATE_TRIAL_{role}_DB" for role in TRIAL_DATABASES}
    if (type(identity_verdicts) is dict and identity_verdicts.keys() == bindings
            and type(origin_tls_attestations) is dict and origin_tls_attestations.keys() == bindings
            and all(identity_verdicts[binding] == "matched"
                    and origin_tls_attestations[binding] is True for binding in bindings)):
        return "qualified"
    return "pending"


def main(*, query=None, build_sha=None, version_id=None, role_pins=None):
    """Write the operator summary and return the two-login preflight status."""
    try:
        scope = parse_scope(os.environ.get(SCOPE_ENV, ""))
        child = evidence_child_env(os.environ[SECRET_ENV], DATABASE)
        if not child["PGPASSWORD"]:
            raise ValueError
        child.update(PGGSSENCMODE="disable", PGPASSFILE="/dev/null")
        manifest_root = Path(__file__).resolve().parents[2]
        baseline, = (entry for entry in load_entries(manifest_root)
                     if entry["file"] == BASELINE and entry["target"] == "app")
        verdicts = observe(child, scope, baseline["sha256"])
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as summary:
            print("### Staging trial roles before application grants\n", file=summary)
            for check, passed in verdicts.items():
                print(f"- {check} — {'pass' if passed else 'refused'}", file=summary)
            if query is not None:
                binding_identities = read_binding_identities(
                    build_sha=build_sha, version_id=version_id, database_oid=scope["database_oid"],
                    role_pins=role_pins, query=query,
                )
                print("\n### Staging trial binding SQL identities\n", file=summary)
                for binding, evidence in binding_identities.items():
                    role = "unconfirmed" if evidence["role"] is None else f"`{evidence['role']}`"
                    print(f"- `{binding}` — {evidence['verdict']} "
                          f"(expected role {role}, expected database `{evidence['database']}`)", file=summary)
    except (KeyError, ValueError, OSError, subprocess.TimeoutExpired, StagingApplyError):
        print("Trial role preflight refused; scope or staging catalog could not be verified.", file=sys.stderr)
        return 1
    return 0 if all(verdicts.values()) else 1


if __name__ == "__main__":
    sys.exit(main())
