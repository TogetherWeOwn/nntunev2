"""Install the private trial overlay with the protected staging migrator."""

import hashlib
from pathlib import Path
import re

import apply_migration as migrator


BASELINE = "0063_browser_playback_receipt.sql"
RECIPE = ".github/scripts/staging_trial_schema.py"
SCHEMAS = (
    "workers/api/tests/fixtures/storage_s3_locator_schema.sql",
    "workers/ingest/tests/fixtures/catalogue_source_schema.sql",
    "workers/api/src/private_library_schema.sql",
)
TABLES = "public.storage_s3_locator_intents, public.catalogue_s3_sources, public.private_library_admissions"
LOCATOR_CHECK = "public.check_storage_s3_locator_intent()"
ROLE_RE = re.compile(r"[A-Za-z0-9_-]{1,63}")


def read_source(root, name):
    path = root / name
    if path.resolve() != path or not path.is_file():
        raise migrator.ApplyError("private trial source must be a regular file in its checkout")
    with path.open("rb") as source:
        contents = source.read(32769)
    if len(contents) > 32768:
        raise migrator.ApplyError("private trial source exceeds the reviewed size bound")
    return contents


def compose(trusted_root, data_root, writer, reader, *, database):
    """Use only fixed, reviewed sources. Return one transaction and its digest."""
    if writer == reader or any(not ROLE_RE.fullmatch(role) or role.lower() == "public"
                               for role in (writer, reader)):
        raise migrator.ApplyError("configure distinct private trial writer and reader roles")
    trusted, data = Path(trusted_root).resolve(), Path(data_root).resolve()
    sources = [read_source(trusted, path) for path in (RECIPE, *SCHEMAS)]
    if any(read_source(data, path) != contents
           for path, contents in zip((RECIPE, *SCHEMAS), sources, strict=True)):
        raise migrator.ApplyError("pinned private trial schema differs from protected main")
    digest = hashlib.sha256(b"".join(hashlib.sha256(part).digest() for part in sources)).hexdigest()
    entries = migrator.load_manifest(trusted / "migrations/manifest.toml")
    baseline = next((entry for entry in entries if entry["file"] == BASELINE and entry["target"] == "app"), None)
    if baseline is None:
        raise migrator.ApplyError("private trial app baseline is missing")
    # Include the chosen roles in the owner-written receipt so a retry with
    # different roles cannot report success while keeping the old grants.
    receipt = f"private-trial {digest} {writer} {reader}"
    locator, source, admission = TABLES.split(", ")
    relations = ", ".join(f"pg_catalog.to_regclass('{table}')" for table in (locator, source, admission))
    exists = f"pg_catalog.num_nonnulls({relations}, pg_catalog.to_regprocedure('{LOCATOR_CHECK}'))"
    body = "\n".join(migrator.transaction_body(part.decode("utf-8")) for part in sources[1:])
    return f"""BEGIN;
{migrator.database_guard_sql(database)}
SELECT pg_catalog.pg_advisory_xact_lock({migrator.ADVISORY_LOCK_KEY});
DO $trial$
BEGIN
    IF NOT EXISTS (SELECT FROM public.schema_migrations
                   WHERE file = '{BASELINE}' AND sha256 = '{baseline['sha256']}') THEN
        RAISE EXCEPTION '{migrator.MISSING_PRIOR}';
    END IF;
    IF pg_catalog.has_schema_privilege('{writer}', 'public', 'CREATE')
       OR pg_catalog.has_schema_privilege('{reader}', 'public', 'CREATE')
       OR NOT pg_catalog.has_schema_privilege('{writer}', 'public', 'USAGE')
       OR NOT pg_catalog.has_schema_privilege('{reader}', 'public', 'USAGE')
       OR pg_catalog.pg_has_role('{reader}', '{writer}', 'MEMBER')
       OR pg_catalog.pg_has_role('{writer}', current_user, 'MEMBER')
       OR pg_catalog.pg_has_role('{reader}', current_user, 'MEMBER') THEN
        RAISE EXCEPTION 'private trial roles must have schema usage and no writer or owner escalation';
    END IF;
    IF {exists} = 4 AND pg_catalog.obj_description(pg_catalog.to_regclass('{admission}'), 'pg_class') = '{receipt}' THEN
        RAISE EXCEPTION '{migrator.ALREADY_SAME}';
    END IF;
    IF {exists} <> 0 THEN
        RAISE EXCEPTION 'private trial schema exists without a matching receipt';
    END IF;
END
$trial$;
{body}
REVOKE ALL ON {TABLES} FROM PUBLIC;
DO $acl$
DECLARE
    grant_row record;
BEGIN
    -- Remove migrator defaults before issuing only the trial grants, including
    -- defaults for the validation function created by the locator schema.
    FOR grant_row IN
        SELECT DISTINCT pg_catalog.pg_get_userbyid(a.grantee) AS role_name,
                        'TABLE' AS kind, c.oid::regclass::text AS object_name
          FROM pg_catalog.pg_class c CROSS JOIN LATERAL pg_catalog.aclexplode(c.relacl) a
         WHERE c.oid IN ({relations}) AND a.grantee NOT IN (0, c.relowner)
        UNION
        SELECT DISTINCT pg_catalog.pg_get_userbyid(a.grantee), 'FUNCTION', '{LOCATOR_CHECK}'
          FROM pg_catalog.pg_proc p CROSS JOIN LATERAL pg_catalog.aclexplode(p.proacl) a
         WHERE p.oid = '{LOCATOR_CHECK}'::regprocedure AND a.grantee NOT IN (0, p.proowner)
    LOOP
        EXECUTE pg_catalog.format('REVOKE ALL ON %s %s FROM %I', grant_row.kind, grant_row.object_name, grant_row.role_name);
    END LOOP;
END
$acl$;
GRANT SELECT ON {TABLES} TO "{reader}", "{writer}";
GRANT INSERT ON {TABLES} TO "{writer}";
GRANT UPDATE (source_job_id, accepted_seq, object_id) ON {source} TO "{writer}";
GRANT UPDATE (accepted_seq, object_id) ON {admission} TO "{writer}";
COMMENT ON TABLE {admission} IS '{receipt}';
COMMIT;
""", digest


def install(child, sql):
    try:
        migrator.psql(child, sql)
    except migrator._AlreadyRecorded as exc:
        if not exc.same:
            raise
        return "already installed"
    return "installed"
