#!/usr/bin/env python3
"""Render the one-off ``schema_migrations`` backfill SQL.

Staging had no migration tracking before 2026-11-03; the operator
verified its contents object-by-object that day. The verified state
is FROZEN below as constants (the manifest as of 2026-11-03, plus the
pre-merge migrations already applied from open PR branches), so
re-rendering later can never quietly rewrite what the dated files
claim to record. The rendered files are committed under ``docs/ops/``
and pinned by a test, so generator and output cannot drift apart.

The app/mirror files record only merged-manifest migrations, so the
staging gate is green immediately after the backfill. The pre-merge
rows live in separate per-PR files: apply a PR's file when that PR
merges (its migration is already on staging), or record equivalently
with ``apply_migration.py --record-only``.
"""
import argparse
import pathlib
import sys

import apply_migration

# The dated artifacts freeze their DDL too. Do not import the live
# runner's DDL: later runner hardening must not rewrite historical SQL.
FROZEN_DDL = """CREATE TABLE IF NOT EXISTS public.schema_migrations (
    file       TEXT PRIMARY KEY,
    sha256     TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    applied_by TEXT NOT NULL
);
REVOKE INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER
    ON TABLE public.schema_migrations FROM PUBLIC;
DO $privileges$
DECLARE
    grant_row record;
BEGIN
    FOR grant_row IN
        SELECT DISTINCT a.grantee
        FROM pg_class c
        JOIN pg_namespace n ON n.oid = c.relnamespace
        CROSS JOIN aclexplode(c.relacl) a
        WHERE n.nspname = 'public' AND c.relname = 'schema_migrations'
          AND a.privilege_type IN
              ('INSERT', 'UPDATE', 'DELETE', 'TRUNCATE', 'REFERENCES', 'TRIGGER')
          AND a.grantee <> c.relowner
    LOOP
        IF grant_row.grantee = 0 THEN
            EXECUTE 'REVOKE INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER '
                 || 'ON TABLE public.schema_migrations FROM PUBLIC';
        ELSE
            EXECUTE format(
                'REVOKE INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER '
                'ON TABLE public.schema_migrations FROM %I',
                pg_get_userbyid(grant_row.grantee));
        END IF;
    END LOOP;
    -- Column privileges live in pg_attribute.attacl, not relacl, so the
    -- table-level pass above cannot see them; revoke per column. Only
    -- INSERT, UPDATE and REFERENCES exist as column-level writes.
    FOR grant_row IN
        SELECT DISTINCT a.grantee, t.attname
        FROM pg_class c
        JOIN pg_namespace n ON n.oid = c.relnamespace
        JOIN pg_attribute t ON t.attrelid = c.oid
            AND t.attnum > 0 AND NOT t.attisdropped
        CROSS JOIN aclexplode(t.attacl) a
        WHERE n.nspname = 'public' AND c.relname = 'schema_migrations'
          AND a.privilege_type IN ('INSERT', 'UPDATE', 'REFERENCES')
          AND a.grantee <> c.relowner
    LOOP
        IF grant_row.grantee = 0 THEN
            EXECUTE format(
                'REVOKE INSERT (%I), UPDATE (%I), REFERENCES (%I) '
                'ON TABLE public.schema_migrations FROM PUBLIC',
                grant_row.attname, grant_row.attname, grant_row.attname);
        ELSE
            EXECUTE format(
                'REVOKE INSERT (%I), UPDATE (%I), REFERENCES (%I) '
                'ON TABLE public.schema_migrations FROM %I',
                grant_row.attname, grant_row.attname, grant_row.attname,
                pg_get_userbyid(grant_row.grantee));
        END IF;
    END LOOP;
END
$privileges$;"""

BACKFILL_AT = "2026-11-03T00:00:00Z"
BACKFILL_BY = "backfill-2026-11-03 (operator-verified)"

# migrations/manifest.toml as verified on 2026-11-03 (freeze; do not
# regenerate from the live manifest — later migrations record
# themselves through apply_migration.py).
FROZEN = {
    "app": [
        ("0002_auth_vault.sql",
         "e8833add4082e4e9951f63af28c37c0d82c50737a60dc572ce86334f565b8587"),
        ("0003_library.sql",
         "d0f04ec912e6e8de47a976d021f3ca3c02aee7f5a18d498cb75d6dff5a90e172"),
        ("0004_auth_challenge_purpose.sql",
         "835b78b01f0d181ea2e6e472bde00e33d19055fa12dfdefeb53782b515a3362e"),
        ("0005_atomic_rate_limits_and_leases.sql",
         "fb61dd289c72b159299e56d0b5a3bfb3734dc5516665a9225c0cce0ffd64bbce"),
        ("0006_account_deletion.sql",
         "6d222588102fbff8bd5096a42e669722262ba84014fc5865bfbf3b8c19894245"),
        ("0007_auth_challenge_user_fk.sql",
         "ce0350f7d08a251bbfab9a817d1c4c3dc72a7484cb2379869c5e6c11a0fb6791"),
        ("0008_auth_challenge_purpose_guard.sql",
         "35508144971de4bdf9919e28a6979963053fa7eb76956df1fd6e98dac004b595"),
        ("0009_metered_admission.sql",
         "e1f5d25c97c2eeb1e292a7761d34f9aaaed43133ddfb536e609a32f8e322f8d8"),
        ("0010_metered_admission_claim.sql",
         "007c434790b5d1433dd0e7f3787409afc9e802092b87aad86d7cd47d8dc87184"),
        ("0011_metered_admission_claim_epoch.sql",
         "50b645b72156218e83ce272c3fdfc97250df422563a2be0d30ad1ecfc72dea02"),
        ("0012_metered_admission_replay_window.sql",
         "6962f20adab4a64672f7c9a870d0e9e606ba8dc638a5b656e8720b0900134a4a"),
        ("0013_auth_challenge_reauth_purpose.sql",
         "848df4805ac18bc1312689f033c86e90417bd9d53bdbfbb0950813b00e9f0d64"),
        ("0015_kek_rotation.sql",
         "c12d9f869611df9d5a4203ba96089aea41c88e3c1e85714dfdcff2e9cf474529"),
        ("0018_qa_issued_codes.sql",
         "f87059e056224631b17463fd99d63cefb060f499c3e41738258e0e43a7244c68"),
        ("0019_native_rest_credentials.sql",
         "0784422be11365318a5c9bf8e66bda1f0f7e233d836a9e809182e28d87da4b56"),
        ("0020_stream_play_ledger.sql",
         "cd67e713c0da150acdcfa43a635a587818aaf6b8db4e7102040985291b6f8b4d"),
        ("0021_billing_schema.sql",
         "ad1e437bff738a7a737ab8cd323fd0a9ff1e74e7e6f08730f186e5b71ab444a1"),
        ("0022_library_artifacts.sql",
         "f28d63c5b585c98aecaa2949c962481a05abf93dff199388ce17f7190434a62a"),
        ("0023_subsonic_clients.sql",
         "38d6c4a05665d4a8149fced4aed3f9025626226d2a0843f39b5a7e052280df12"),
        ("0024_session_refresh_retry.sql",
         "d4fc42468d1260df227c5b4b0b4721165429fb51c13a0265d4fadcb9931dc47e"),
        ("0025_session_refresh_claim_clock.sql",
         "120a35f3515e0c48e1df882f1eed17bb5eed425d9d91bf5da8ec86ec1f9318af"),
        ("0026_plan_reseed_no_trial.sql",
         "b3668efdfd27bd81d9a9f86441c29925963943c30ed969303bd518af91f30b0c"),
        ("0027_track_sources_global.sql",
         "67ebbc16347589e856d68bcf17f939045b79de8fbc8ef0fc179b22ca70094ade"),
        ("0028_admin_foundation.sql",
         "20fafb4309a6829c07a679efd052000f7711398ff22bbe346203ba1522e68754"),
        ("0029_paid_invoice_history.sql",
         "ae1ff5ab6c7f4d060667aa1442d1a76dfa860bec0ddba86d83400462ee3131f8"),
    ],
    "mirror": [
        ("0001_mb_mirror.sql",
         "47309207c2e87761477e4b74884454afd5c68916477be64a9c3bbc2d105723c3"),
        ("0014_mb_import_resume_dlq.sql",
         "8385253fbddb464361f30678cfb8464b7c3d73f7ae5614f0eced870876dcef88"),
        ("0016_mb_track.sql",
         "4e984b85eb9d62a498402dc2ddf9474ac8297fe988b22cbfc1434bd8c7cbdbaa"),
        ("0017_mb_replication_packet_ledger.sql",
         "62ceb24df9c5ea7032fc3cf4e48e5e10f87d909639fa469ed79b4dfcd92009d6"),
    ],
}

# Applied to nntunev2_staging on 2026-11-03 from open PR branches, before
# either PR merged. Whichever merges second renumbers byte-identically
# and records these original name+sha pairs as `renamed_from`, so the
# rows keep satisfying the renamed entries. sha256 values are the
# PR-head file bytes verified on 2026-11-03. NOT part of the app
# backfill: recording them before their PR merges would (correctly)
# fail the staging gate as unlisted rows.
APPLIED_BEFORE_MERGE = [
    # TogetherWeOwn/nntunev2#922 (feat/admin-invites @ 074dff75)
    ("0030_invite_codes.sql",
     "12c4331ee4a69688d12352699db634d46a11a4f68814b31a6db572ecdcf01cda", "922"),
    # TogetherWeOwn/nntunev2#917 (feat/meter-derivation-leases @ a18d3d78)
    ("0030_meter_derivation_and_leases.sql",
     "04aba1facde443795a15f86aae18791b030ef0ecbb2b61e14e797317dbc09808", "917"),
    ("0031_ledger_seconds_floor.sql",
     "f5d15c8e8af8092ac3d13a9a296bf412ad9e84ae4e3f01de65db0e159e60fe34", "917"),
]

TARGET_NOTES = {
    "app": "Apply to the app database (staging: nntunev2_staging) with its migrator role.",
    "mirror": "Apply to the MusicBrainz mirror database with its migrator role. "
              "The mirror is shared by staging and production reads, so this "
              "file is applied only under a Release Manager decision.",
}

TARGETS = ("app", "mirror", "premerge-917", "premerge-922")


def rows_for(target):
    if target.startswith("premerge-"):
        pr = target.removeprefix("premerge-")
        return sorted((name, sha) for name, sha, row_pr in APPLIED_BEFORE_MERGE
                      if row_pr == pr)
    return sorted(FROZEN[target])


def render(target):
    if target.startswith("premerge-"):
        pr = target.removeprefix("premerge-")
        note = (f"Apply to nntunev2_staging when PR #{pr} merges (its migration "
                "is already applied there); do not apply before merge. "
                "Equivalent: apply_migration.py --record-only per file.")
    else:
        note = TARGET_NOTES[target]
    lines = [
        "-- GENERATED by .github/scripts/schema_migrations_backfill.py"
        f" --target {target}; do not edit by hand.",
        "-- One-off backfill for schema_migrations: records the",
        "-- operator-verified staging state of 2026-11-03. Idempotent:",
        "-- rows already present are left untouched.",
        f"-- {note}",
        "",
        "BEGIN;",
        FROZEN_DDL,
        "",
    ]
    for filename, sha256 in rows_for(target):
        lines.append(apply_migration.insert_sql(
            filename, sha256, BACKFILL_BY, applied_at=BACKFILL_AT) + "\n")
    lines.append("COMMIT;")
    return "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--check", metavar="PATH",
                        help="exit 1 unless PATH already equals the rendered SQL")
    args = parser.parse_args(argv)
    sql = render(args.target)
    if args.check:
        existing = pathlib.Path(args.check).read_text(encoding="utf-8")
        if existing != sql:
            print(f"schema-migrations-backfill: {args.check} is stale; regenerate it",
                  file=sys.stderr)
            return 1
        print(f"schema-migrations-backfill: {args.check} is current")
        return 0
    print(sql, end="")
    return 0


if __name__ == "__main__":
    sys.exit(main())
