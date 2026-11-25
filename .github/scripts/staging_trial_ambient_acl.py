"""Prepare and restore database ACLs before isolated trial logins exist."""

import hashlib
import http.client
import json
import os
import re
import subprocess
import sys

from apply_migration import ADVISORY_LOCK_KEY, ApplyError
from staging_apply import StagingApplyError, evidence_child_env, quote_literal as literal
from staging_trial_isolated import SHARED_OID
from staging_trial_role_acl import audit, require
from staging_trial_role_lifecycle import NAMES, baseline, psql, target
from staging_trial_role_preflight import DATABASE, ROLE_RE, oid


DATABASE_OID = 16494
PRIVILEGES = {"CONNECT", "CREATE", "TEMPORARY"}
OPERATIONS = {"check-ambient", "restrict-ambient", "restore-ambient"}
CATALOG = """WITH db AS (SELECT * FROM pg_database WHERE datallowconn),
acl AS (SELECT d.oid AS dbid, a.* FROM db d,
        LATERAL aclexplode(coalesce(d.datacl, acldefault('d', d.datdba))) a)
SELECT jsonb_build_object(
 'databases', coalesce((SELECT jsonb_agg(jsonb_build_array(oid::bigint, datname, datdba::bigint, datistemplate)
                         ORDER BY oid) FROM db), '[]'),
 'acl', coalesce((SELECT jsonb_agg(jsonb_build_array(dbid::bigint, grantee::bigint, grantor::bigint,
                         privilege_type, is_grantable)
                  ORDER BY dbid, grantee, grantor, privilege_type, is_grantable) FROM acl), '[]'))"""


def branch_pin(database, branch, branch_id):
    return (database.get("default_branch") == "main"
            and branch.get("id") == branch_id and branch.get("name") == "staging"
            and branch.get("production") is False and branch.get("default", False) is False
            and branch.get("ready") is True)


def verify_branch(branch_id, token):
    if not re.fullmatch(r"[A-Za-z0-9_-]{1,128}:[A-Za-z0-9_-]{1,512}", token):
        raise ValueError
    conn = http.client.HTTPSConnection("api.planetscale.com", timeout=8)
    route = "/v1/organizations/togetherweown/databases/nntunev2"
    headers = {"Authorization": token, "Accept": "application/json"}
    try:
        metadata = []
        for path in (route, f"{route}/branches/staging"):
            conn.request("GET", path, headers=headers)
            resp = conn.getresponse()
            if resp.status != 200:
                raise StagingApplyError("isolated provider branch identity")
            body = resp.read(32769)
            if len(body) > 32768:
                raise ValueError
            metadata.append(json.loads(body))
        if not branch_pin(*metadata, branch_id):
            raise StagingApplyError("isolated provider branch identity")
    finally:
        conn.close()


def plan_config(raw, digest):
    if len(raw) > 32000 or hashlib.sha256(raw.encode()).hexdigest() != digest:
        raise ValueError
    plan = json.loads(raw)
    if (plan.keys() != {"branch_id", "direct", "port", "baseline", "consumers"}
            or not re.fullmatch(r"[a-z0-9]{1,63}", plan["branch_id"])
            or not re.fullmatch(r"[a-z0-9.-]+\.psdb\.cloud", plan["direct"])
            or type(plan["port"]) is not int or plan["port"] != 5432):
        raise ValueError
    return plan


def after_plan(original, consumers, database_oid):
    if original.keys() != {"databases", "acl"} or len(original["databases"]) > 64:
        raise ValueError
    dbs = {}
    for dbid, name, owner, template in original["databases"]:
        if (not oid(dbid) or dbid in dbs or not ROLE_RE.fullmatch(name) or not oid(owner)
                or type(template) is not bool):
            raise ValueError
        dbs[dbid] = (name, owner, template)
    if database_oid == SHARED_OID or dbs.get(database_oid, (None,))[0] != DATABASE:
        raise ValueError
    rows = set()
    for dbid, grantee, grantor, priv, option in original["acl"]:
        if (dbid not in dbs or not oid(grantee, optional=True) or not oid(grantor)
                or priv not in PRIVILEGES or type(option) is not bool):
            raise ValueError
        rows.add((dbid, grantee, grantor, priv, option))
    public = {row for row in rows if row[1] == 0}
    exposed = {(d, p) for d, _, _, p, _ in public}
    added, identities = set(), set()
    direct = {(d, r, p) for d, r, _, p, _ in rows}
    for dbid, grantee, name, priv in consumers:
        if ((dbid, priv) not in exposed or not oid(grantee) or not ROLE_RE.fullmatch(name)
                or name.lower() == "public" or name in NAMES):
            raise ValueError
        identities.add((grantee, name))
        if (dbid, grantee, priv) not in direct:
            added.add((dbid, grantee, dbs[dbid][1], priv, False))
    after = {**original, "acl": sorted(rows - public | added)}
    return dbs, after, sorted(public), sorted(added), sorted(identities)


def compose(action, original, consumers, database_oid, login, *, fixture=False):
    dbs, after, public, added, identities = after_plan(original, consumers, database_oid)
    owner = dbs[database_oid][1]
    affected = sorted({row[0] for row in public})
    if any(grantor != owner or dbs[dbid][1] != owner or option
           for dbid, _, grantor, _, option in public):
        raise StagingApplyError("isolated database ACL grantor authority")

    def state(value):
        return f"({CATALOG}) = {literal(json.dumps(value, ensure_ascii=True))}::jsonb"

    def ddl(rows, grant):
        verb, direction = ("GRANT", "TO") if grant else ("REVOKE", "FROM")
        suffix = "" if grant else " RESTRICT"
        return "".join(
            f"SELECT format('{verb} {p} ON DATABASE %I {direction} "
            + (f"%I{suffix}', {literal(dbs[d][0])}, pg_get_userbyid({r})" if r else
               f"PUBLIC{suffix}', {literal(dbs[d][0])}") + ") \\gexec\n"
            for d, r, _, p, _ in rows
        )

    def gate_sessions():
        return require(f"""NOT EXISTS (SELECT FROM pg_stat_activity s WHERE s.datid IN ({','.join(map(str, affected)) or 'NULL'})
              AND s.backend_type = 'client backend'
              AND NOT coalesce(has_database_privilege(s.usesysid, s.datid, 'CONNECT'), false))""")

    readonly = action == "check-ambient"
    sql = f"BEGIN {'READ ONLY' if readonly else 'READ WRITE'};\nSET LOCAL standard_conforming_strings = on;\n"
    sql += target({"database_oid": database_oid}, login, fixture) + baseline()
    sql += f"SELECT pg_advisory_xact_lock({ADVISORY_LOCK_KEY});\n"
    sql += require(f"to_regrole(current_user)::oid = {owner}")
    sql += require(f"NOT EXISTS (SELECT FROM pg_roles WHERE rolname IN ({', '.join(map(literal, NAMES))}))")
    for grantee, name in identities:
        sql += require(f"EXISTS (SELECT FROM pg_roles WHERE oid = {grantee} AND rolname = {literal(name)} AND rolcanlogin)")
    before, restricted = state(original), state(after)
    sql += require(f"({before}) OR ({restricted})")
    if action != "restore-ambient":
        sql += audit([], exact=False, database_preparation=True)
        for dbid, _, _, priv, _ in public:
            sql += require(f"has_database_privilege({dbid}::oid, '{priv} WITH GRANT OPTION')")
        connect = ",".join(f"({d}::oid,{r}::oid)" for d, r, _, p, _ in after["acl"] if p == "CONNECT")
        connect = connect or "(NULL::oid,NULL::oid)"
        if affected:
            sql += require(f"""NOT EXISTS (SELECT FROM pg_stat_activity s
              WHERE s.datid IN ({','.join(map(str, affected))}) AND s.backend_type = 'client backend'
                AND NOT EXISTS (SELECT FROM pg_roles WHERE oid = s.usesysid AND rolsuper)
                AND NOT EXISTS (SELECT FROM (VALUES {connect}) AS c(db, grantee)
                  WHERE c.db = s.datid AND pg_has_role(s.usesysid, c.grantee, 'USAGE')))""")
    if action == "restrict-ambient":
        sql += f"SELECT {before} AS change_needed \\gset\n\\if :change_needed\n"
        sql += ddl(added, True) + ddl(public, False) + "\\endif\n"
        sql += require(restricted) + "SELECT pg_stat_clear_snapshot();\n" + gate_sessions() + audit([], exact=False)
    if action == "restore-ambient":
        sql += f"SELECT {restricted} AS change_needed \\gset\n\\if :change_needed\n"
        sql += ddl(public, True) + ddl(added, False) + "\\endif\n" + require(before)
    return sql + "COMMIT;\n"


def main():
    try:
        action, = sys.argv[1:]
        if action not in OPERATIONS:
            raise ValueError
        plan = plan_config(os.environ["AMBIENT_PLAN"], os.environ["AMBIENT_PLAN_SHA256"])
        child = evidence_child_env(os.environ["PRIVATE_TRIAL_OWNER_URL"], DATABASE)
        login = f"postgres.{plan['branch_id']}"
        if (child["PGHOST"], child["PGPORT"], child["PGUSER"]) != (plan["direct"], str(plan["port"]), login) or not child["PGPASSWORD"]:
            raise ValueError
        verify_branch(plan["branch_id"], os.environ["ISOLATED_TRIAL_BRANCH_READ"])
        child.update(PGGSSENCMODE="disable", PGPASSFILE="/dev/null")
        sql = compose(action, plan["baseline"], plan["consumers"], DATABASE_OID, login)
        psql(child, sql, f"isolated staging {action}")
    except (ApplyError, StagingApplyError, AttributeError, KeyError, TypeError, ValueError, OSError,
            http.client.HTTPException, subprocess.TimeoutExpired):
        print("Isolated ambient ACL operation refused. Reconcile the baseline and grantor scope.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
