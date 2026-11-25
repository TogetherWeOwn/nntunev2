"""Exact direct grants and effective PUBLIC access for the private staging trial."""

from staging_apply import quote_literal as literal
from staging_trial_schema import TABLES

# Empty column lists mean an object-level grant. All base table grants are
# column-scoped, including the full reviewed winner projection in 0052.
COMMON = {
    "users": "id deleting_at role invite_code_hash email",
    "invite_codes": "id kind issued_by code_hash email used redeemed_by redeemed_at expires_at revoked_at",
    "storage_validated_digest_references": "environment_id library_id reference_id object_id",
    "storage_library_namespaces": "id environment_id owner_id",
}
READER = {
    "tracks": "id mbid release_id title duration_ms track_no medium_no",
    "playback_leases": "user_id session_id track_id play_id epoch expires_at continuation_epoch continuation_deadline",
    "storage_validated_digest_winners": (
        "object_id environment_id library_id plaintext_bytes plaintext_sha256 root_version_id "
        "quarantined retain_until delete_until deleting_at"
    ),
    "storage_object_intents": "object_id generation_id provider_configuration_version_id",
    "storage_digest_map_intents": "object_id map_id",
    "storage_digest_inventory_intents": "map_id media_version",
}
WRITER = {
    "sessions": "id user_id created_at expires_at rotation_counter revoked",
    "ingest_jobs": "id user_id status accepted_seq",
    "tracks": "id release_id medium_no track_no mbid duration_ms",
    "storage_validated_digest_winners": READER["storage_validated_digest_winners"] + " read_until upload_until",
    "storage_object_intents": (
        "object_id environment_id library_id plaintext_bytes generation_id provider_configuration_version_id header_tag"
    ),
    "storage_digest_map_intents": "object_id environment_id library_id plaintext_bytes map_id",
    "storage_digest_inventory_intents": "map_id media_version page_versions",
}
WINNERS = "public.storage_validated_digest_winners"
LOCK_TABLES = ("users", "sessions", "invite_codes", "ingest_jobs", "tracks")
GUARD = "public.reject_private_trial_publication_update()"
ROUTINES = (
    "public.lock_storage_digest_winner(uuid,uuid,bytea,bigint,uuid,uuid,uuid)",
    "public.attach_storage_digest_reference(uuid,uuid,bytea,bigint,uuid,uuid,uuid,uuid,timestamptz)",
)
# Existing PUBLIC invoker routines from the app baseline. These are not
# managed grants. Triggers cannot be called directly and the reservation
# helpers have no underlying relation privileges for either trial role.
AMBIENT = (
    "auth_challenges_purpose_immutable()", "guard_ingest_audio_manifest()",
    "guard_ingest_audio_classifications()", "storage_namespace_immutable()",
    "storage_is_uuid_v4(uuid)", "storage_object_intent_append_only()",
    "storage_digest_reservation_guard()", "reserve_storage_digest(uuid,integer)",
    "release_storage_digest(uuid,bigint)",
)
KINDS = {
    "TABLE": ("pg_class", "regclass"), "FUNCTION": ("pg_proc", "regprocedure"),
    "SCHEMA": ("pg_namespace", "regnamespace"), "DATABASE": ("pg_database", None),
}


def grants(database, scope, *, locks=False, overlay=False):
    for key, selects in (("reader", READER), ("writer", WRITER)):
        name, oid = scope[key]
        yield name, oid, "DATABASE", database, "CONNECT", ""
        yield name, oid, "SCHEMA", "public", "USAGE", ""
        for table, columns in (COMMON | selects).items():
            yield name, oid, "TABLE", f"public.{table}", "SELECT", columns
        if overlay:
            for table in TABLES.split(", "):
                yield name, oid, "TABLE", table, "SELECT", ""
                if key == "writer":
                    yield name, oid, "TABLE", table, "INSERT", ""
    name, oid = scope["writer"]
    yield name, oid, "TABLE", "public.tracks", "INSERT", "release_id medium_no track_no mbid title duration_ms"
    refs = "storage_validated_digest_references"
    yield name, oid, "TABLE", f"public.{refs}", "INSERT", COMMON[refs]
    yield name, oid, "TABLE", WINNERS, "UPDATE", "object_id"
    for routine in ROUTINES:
        yield name, oid, "FUNCTION", routine, "EXECUTE", ""
    if locks:
        for table in LOCK_TABLES:
            yield name, oid, "TABLE", f"public.{table}", "UPDATE", "id"
    if overlay:
        yield name, oid, "TABLE", "public.catalogue_s3_sources", "UPDATE", "source_job_id accepted_seq object_id"
        yield name, oid, "TABLE", "public.private_library_admissions", "UPDATE", "accepted_seq object_id"


def column_list(columns):
    return f" ({', '.join(columns.split())})" if columns else ""


def change(entries, verb):
    direction = {"GRANT": "TO", "REVOKE": "FROM"}[verb]
    return "\n".join(
        f'{verb} {priv}{column_list(cols)} ON {kind} {obj} {direction} "{name}";'
        for name, _, kind, obj, priv, cols in entries
    )


def require(test):
    return f"DO $check$ BEGIN IF ({test}) IS NOT TRUE THEN RAISE EXCEPTION 'contract refused'; END IF; END $check$;\n"


# The only approved installed extension is the catalog's PL/pgSQL.
ISOLATED_EXTENSION_CATALOG = """SELECT count(*) = 1 AND bool_and(e.extname = 'plpgsql'
             AND e.extversion = '1.0' AND n.nspname = 'pg_catalog')
             FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace"""


def isolated_extensions():
    return require(ISOLATED_EXTENSION_CATALOG)


def other_database_connections(role):
    return f"""SELECT FROM pg_database d WHERE d.datallowconn AND d.datname <> current_database()
              AND has_database_privilege({role}, d.oid, 'CONNECT')"""


def audit(entries, *, exact, database_preparation=False):
    """Without role inheritance or ownership, direct plus PUBLIC is effective."""
    expected = []
    for _, oid, kind, obj, privilege, columns in entries:
        catalog, cast = KINDS[kind]
        obj_id = f"{literal(obj)}::{cast}::oid" if cast else (
            f"(SELECT oid FROM pg_database WHERE datname = {literal(obj)})"
        )
        for col in columns.split() or [None]:
            sub = (f"(SELECT attnum FROM pg_attribute WHERE attrelid = {obj_id} "
                   f"AND attname = {literal(col)} AND NOT attisdropped)") if col else "0"
            expected.append(f"({oid}::oid,'{catalog}'::regclass::oid,{obj_id},{sub},{literal(privilege)},false)")
    values = "VALUES " + ",\n".join(expected) if expected else (
        "SELECT NULL::oid, NULL::oid, NULL::oid, NULL::integer, NULL::text, NULL::boolean WHERE false"
    )
    ids = ",".join(str(oid) for oid in sorted({entry[1] for entry in entries})) or "NULL"
    routines = ", ".join(f"{literal(sig)}::regprocedure" for sig in ROUTINES)
    ambient = ",".join(f"to_regprocedure('public.{sig}')" for sig in AMBIENT)
    missing = "AND NOT EXISTS (SELECT * FROM expected EXCEPT SELECT * FROM direct)" if exact else ""
    return require(f"""
WITH expected AS (SELECT * FROM ({values}) e(role_id, class_id, obj, sub, priv, opt)
                  WHERE role_id IN (SELECT oid FROM pg_roles)),
schemas AS (SELECT * FROM pg_namespace WHERE nspname !~ '^pg_' AND nspname <> 'information_schema'),
relations AS (SELECT c.* FROM pg_class c JOIN schemas s ON s.oid = c.relnamespace),
routines AS (SELECT p.* FROM pg_proc p JOIN schemas s ON s.oid = p.pronamespace),
objects(class_id, obj, sub, acl) AS (
    SELECT 'pg_database'::regclass::oid, oid, 0, COALESCE(datacl, acldefault('d',datdba))
      FROM pg_database WHERE datname = current_database()
    UNION ALL SELECT 'pg_namespace'::regclass, oid, 0, nspacl FROM schemas
    UNION ALL SELECT 'pg_class'::regclass, oid, 0, relacl FROM relations
    UNION ALL SELECT 'pg_class'::regclass, c.oid, a.attnum, a.attacl
      FROM relations c JOIN pg_attribute a ON a.attrelid = c.oid WHERE a.attnum > 0 AND NOT a.attisdropped
    UNION ALL SELECT 'pg_proc'::regclass, oid, 0, COALESCE(proacl, acldefault('f',proowner)) FROM routines
),
acl AS (SELECT a.grantee AS role_id, o.class_id, o.obj, o.sub, a.privilege_type AS priv, a.is_grantable AS opt
          FROM objects o CROSS JOIN LATERAL aclexplode(o.acl) a),
direct AS (SELECT * FROM acl WHERE role_id IN ({ids}))
SELECT NOT EXISTS (SELECT * FROM direct EXCEPT SELECT * FROM expected)
  {missing}
  AND NOT EXISTS (SELECT FROM expected WHERE sub IS NULL)
  AND NOT EXISTS (
    SELECT FROM acl a WHERE role_id = 0 AND NOT (
        (class_id = 'pg_namespace'::regclass AND obj = 'public'::regnamespace AND priv = 'USAGE')
        OR ({str(database_preparation).lower()} AND class_id = 'pg_database'::regclass)
        OR (class_id = 'pg_proc'::regclass AND priv = 'EXECUTE'
            AND obj IN (SELECT oid FROM routines WHERE NOT prosecdef AND oid IN ({ambient})))
    ))
  AND NOT EXISTS (
    SELECT FROM pg_shdepend d WHERE refclassid = 'pg_authid'::regclass AND refobjid IN ({ids})
      AND NOT (deptype = 'a' AND dbid IN (0, (SELECT oid FROM pg_database WHERE datname = current_database()))
        AND EXISTS (SELECT FROM direct a WHERE a.role_id = d.refobjid
                    AND (a.class_id, a.obj, a.sub) = (d.classid, d.objid, d.objsubid)))
    )
  AND NOT EXISTS (
    SELECT FROM pg_roles r WHERE r.oid IN ({ids}) AND EXISTS ({other_database_connections('r.oid')})
    )
  AND NOT EXISTS (SELECT FROM pg_proc WHERE oid IN ({routines})
                  AND (prosecdef OR prokind <> 'f' OR proconfig IS DISTINCT FROM ARRAY['search_path=pg_catalog, public']))
""")


def identities(scope, *, missing=False, login=True):
    reader, writer = scope["reader"], scope["writer"]
    ids = f"{reader[1]}, {writer[1]}"
    requested = f"(VALUES ('{reader[0]}',{reader[1]}::oid),('{writer[0]}',{writer[1]}::oid))"
    return require(f"""
WITH requested(name, id) AS ({requested})
SELECT NOT EXISTS (
    SELECT FROM requested q LEFT JOIN pg_roles r ON q.name = r.rolname::text AND q.id = r.oid
    WHERE {'false' if missing else 'r.oid IS NULL'}
       OR r.rolsuper OR r.rolinherit OR r.rolcreaterole OR r.rolcreatedb OR r.rolreplication
       OR r.rolbypassrls {'OR NOT r.rolcanlogin' if login else ''}
)
AND NOT EXISTS (
    SELECT FROM pg_roles r WHERE (oid,rolname::text) NOT IN (SELECT id,name FROM requested)
      AND (oid IN ({ids}) OR rolname IN (SELECT name FROM requested))
)
AND NOT EXISTS (SELECT FROM pg_db_role_setting WHERE setrole IN ({ids}))
AND NOT EXISTS (
    SELECT FROM pg_auth_members m WHERE member IN ({ids}) OR (roleid IN ({ids}) AND NOT (
       member = {scope['issuer_oid']} AND admin_option AND NOT (inherit_option OR set_option)
       AND EXISTS (SELECT FROM pg_roles WHERE oid = m.member AND rolcreaterole))))
""")
