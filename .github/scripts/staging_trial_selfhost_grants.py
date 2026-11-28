"""Install and verify the separate stock PostgreSQL trial caller ACLs."""

import hashlib
import os
from pathlib import Path
import subprocess
import sys

from apply_migration import ADVISORY_LOCK_KEY, ApplyError, transaction_body
from staging_apply import StagingApplyError
from staging_trial_isolated import ROLES, SERVING, selfhost_target_config
from staging_trial_role_acl import (
    GUARD, ROUTINES, audit, change, grants as publication_grants, identities, isolated_extensions, require,
)
from staging_trial_role_lifecycle import baseline, lock_contract, psql, target
from staging_trial_role_preflight import DATABASE, oid
from staging_trial_schema import LOCATOR_CHECK, SCHEMAS, TABLES, read_source
from staging_trial_selfhost import session_guard, tls_connection


# Entries describe the callers' SQL, including the projections and lock-only
# columns required by invoker routines. The same entries drive the ACL audit.
ACCESS = {
    "auth": [
        ("native_rest_credentials", "SELECT", "id user_id key_prefix key_hmac pepper_version scopes created_at expires_at revoked_at"),
        ("ingest_session_reservations", "SELECT", "id principal_id library_id fingerprint terminal_reason"),
        ("storage_library_namespaces", "SELECT", "id environment_id owner_id"),
    ],
    "enroll": [
        ("ingest_jobs", "SELECT", "id user_id status accepted_seq"),
        ("ingest_jobs", "UPDATE", "id"),
        ("storage_library_namespaces", "SELECT", "id environment_id owner_id"),
        ("ingest_session_reservations", "SELECT", "id absolute_deadline"),
        ("ingest_session_reservations", "INSERT", "id library_id principal_id idempotency_key fingerprint absolute_deadline idle_deadline"),
        ("ingest_session_cleanup_intents", "INSERT", "session_id kind due_at"),
        ("storage_object_intents", "INSERT",
         "object_id generation_id environment_id library_id provider_configuration_version_id plaintext_bytes envelope_bytes header_tag"),
        ("storage_plaintext_digest_claim_intents", "INSERT SELECT", "object_id environment_id library_id plaintext_bytes plaintext_sha256"),
        ("storage_digest_reservations", "SELECT INSERT", "environment_id library_id plaintext_sha256 plaintext_bytes object_id generation expires_at"),
        ("storage_digest_reservations", "UPDATE", "object_id generation expires_at"),
        ("storage_digest_map_intents", "INSERT", "object_id environment_id library_id plaintext_bytes map_id"),
        ("storage_digest_map_page_intents", "INSERT", "map_id chunk_count page_index expected_hash"),
        ("storage_object_envelope_intents", "INSERT", "object_id root_version_id wrapped_record"),
    ],
    "upload": [
        ("storage_library_namespaces", "SELECT", "id environment_id"),
        ("storage_object_intents", "SELECT",
         "object_id generation_id environment_id library_id provider_configuration_version_id plaintext_bytes header_tag"),
        ("ingest_session_reservations", "SELECT", ""),
        ("ingest_session_reservations", "UPDATE",
         "idle_deadline checkpoint_generation checkpoint_revision checkpoint_offset write_lease_id write_lease_generation "
         "terminal_reason terminal_at multipart_object_id multipart_bucket multipart_upload_id completed_map_id"),
        ("ingest_session_cleanup_intents", "INSERT", "session_id kind due_at"),
        ("storage_digest_reservations", "SELECT", "environment_id library_id plaintext_sha256 plaintext_bytes object_id generation expires_at"),
        ("storage_digest_reservations", "UPDATE", "environment_id"),
        ("storage_digest_map_intents", "SELECT", "object_id environment_id library_id plaintext_bytes map_id page_count"),
        ("storage_digest_map_page_intents", "SELECT", "expected_hash map_id page_index"),
        ("storage_object_envelope_intents", "SELECT", "object_id root_version_id wrapped_record"),
        ("storage_digest_inventory_intents", "SELECT INSERT", "map_id media_version page_versions"),
        ("storage_validated_digest_winners", "SELECT", ""),
        ("storage_validated_digest_winners", "UPDATE", "upload_until"),
        ("storage_validated_digest_winners", "INSERT",
         "object_id environment_id library_id plaintext_bytes plaintext_sha256 root_version_id retain_until"),
    ],
}
RESERVE = "public.reserve_storage_digest(uuid,integer)"
FUNCTIONS = {
    "auth": (),
    "enroll": (RESERVE, "public.store_storage_digest_candidate(uuid,uuid,uuid,uuid,bigint,bytea[])"),
    "upload": ROUTINES[:1],
}


def install_sql(root, pins):
    role_oids = pins["role_oids"]
    if not all(oid(role_oids[role]) for role in SERVING):
        raise ValueError
    scope = {"issuer_oid": 0, **{role: (ROLES[role], role_oids[role]) for role in SERVING}}
    base = [(*scope[role], "DATABASE", DATABASE, "CONNECT", "") for role in SERVING]
    grants = [(*scope[role], "SCHEMA", "public", "USAGE", "") for role in ACCESS]
    for role, entries in ACCESS.items():
        grants.extend((*scope[role], "TABLE", f"public.{table}", privilege, columns)
                      for table, privileges, columns in entries for privilege in privileges.split())
        grants.extend((*scope[role], "FUNCTION", function, "EXECUTE", "") for function in FUNCTIONS[role])
    publisher = [entry for entry in publication_grants(
        DATABASE, {"reader": scope["auth"], "writer": scope["publish"]}, locks=True, overlay=True,
    ) if entry[0] == ROLES["publish"] and entry[2] != "DATABASE"]
    publish_locks = [entry for entry in publisher if entry[4:] == ("UPDATE", "id")]
    grants.extend(entry for entry in publisher if entry not in publish_locks)
    sources = [read_source(root, path) for path in (*SCHEMAS, ".github/scripts/staging_trial_selfhost_grants.py")]
    digest = hashlib.sha256(b"".join(hashlib.sha256(source).digest() for source in sources)).hexdigest()
    receipt = "selfhost-private-trial " + digest + " " + " ".join(str(role_oids[role]) for role in SERVING)
    relations = ", ".join(f"to_regclass('{table}')" for table in TABLES.split(", "))
    count = f"num_nonnulls({relations}, to_regprocedure('{LOCATOR_CHECK}'))"
    enroll_oid = role_oids["enroll"]
    enroll_when = f"CURRENT_USER = pg_catalog.pg_get_userbyid('{enroll_oid}'::pg_catalog.oid)"
    sql = "BEGIN READ WRITE;\nSET LOCAL lock_timeout = '5s';\nSET LOCAL statement_timeout = '30s';\n"
    sql += target(pins, ROLES["executor"], False) + session_guard(pins, "executor") + baseline()
    sql += f"SELECT pg_advisory_xact_lock({ADVISORY_LOCK_KEY});\n"
    sql += isolated_extensions() + identities(scope, keys=SERVING)
    sql += f"SELECT {count} = 0 AS new_overlay \\gset\n\\if :new_overlay\n"
    sql += audit(base, exact=True, include_closed=True)
    sql += "\n".join(transaction_body(source.decode()) for source in sources[:-1])
    sql += f"""
SET LOCAL search_path = pg_catalog;
REVOKE EXECUTE ON FUNCTION {RESERVE}, public.release_storage_digest(uuid,bigint) FROM PUBLIC;
{lock_contract({"writer": scope["publish"]}, "grant")}
CREATE TRIGGER private_trial_enroll_lock_only BEFORE UPDATE ON public.ingest_jobs
    FOR EACH STATEMENT WHEN ({enroll_when}) EXECUTE FUNCTION {GUARD};
ALTER TABLE public.ingest_jobs ENABLE ALWAYS TRIGGER private_trial_enroll_lock_only;
{change(grants, "GRANT")}
COMMENT ON TABLE public.private_library_admissions IS '{receipt}';
\\endif
"""
    sql += require(f"{count} = 4 AND obj_description('public.private_library_admissions'::regclass, 'pg_class') = '{receipt}'")
    sql += lock_contract({"writer": scope["publish"]}, "verify")
    sql += require(f"""SELECT tgenabled = 'A' AND tgtype = 18 AND tgfoid = '{GUARD}'::regprocedure
        AND pg_get_expr(tgqual, tgrelid) = format('(CURRENT_USER = pg_get_userbyid(%L::oid))', '{enroll_oid}')
        FROM pg_trigger WHERE tgrelid = 'public.ingest_jobs'::regclass AND tgname = 'private_trial_enroll_lock_only'""")
    sql += audit(base + grants + publish_locks, exact=True, include_closed=True)
    sql += require(f"NOT has_function_privilege('{ROLES['auth']}', '{RESERVE}', 'EXECUTE') AND "
                   f"NOT has_function_privilege('{ROLES['auth']}', 'public.release_storage_digest(uuid,bigint)', 'EXECUTE')")
    return sql + "COMMIT;\n"


def main():
    try:
        pins = selfhost_target_config(os.environ["SELFHOST_TRIAL_PINS"])
        child = tls_connection(os.environ["PRIVATE_TRIAL_EXECUTOR_URL"], pins, "direct", "executor",
                               Path(os.environ["SELFHOST_TRIAL_CA_FILE"]))
        psql(child, install_sql(Path(__file__).resolve().parents[2], pins), "stock trial ACL install")
    except (ApplyError, StagingApplyError, KeyError, ValueError, OSError, subprocess.TimeoutExpired):
        print("Stock trial ACL installation refused.", file=sys.stderr)
        return 1
    print("Stock trial caller ACLs verified.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
