#!/usr/bin/env python3
"""Execute the ingest CAS statement against the migrated global source shape.

DATABASE_URL must be disposable. All data and prepared statements are rolled back;
temporary copies retain migrated column types/CHECKs without production FKs.
"""

import os
from pathlib import Path
import re
import subprocess


ROOT = Path(__file__).resolve().parents[3]
SOURCE = ROOT / "workers/ingest/src/pointer_switch.rs"


def main():
    database_url = os.environ["DATABASE_URL"]
    match = re.search(
        r'pub fn switch_track_source_sql\(\) -> &\'static str \{\s*"([^"\n]+)"\s*\}',
        SOURCE.read_text(),
    )
    if not match:
        raise AssertionError("could not extract the exact switch statement")
    statement = match.group(1)
    setup = """
BEGIN;
CREATE TEMP TABLE users AS SELECT id, deleting_at FROM public.users WITH NO DATA;
CREATE TEMP TABLE track_sources (LIKE public.track_sources INCLUDING ALL);
INSERT INTO users VALUES ('open-fence', NULL), ('deleting-fence', now());
PREPARE switch_source (text, text, text, bytea, integer, bytea, bytea, bytea, smallint, text) AS
""" + statement + ";\n"
    reset = """
TRUNCATE track_sources;
INSERT INTO track_sources
    (track_id, source_job_id, nzb_ref_encrypted, nzb_ref_envelope_version,
     nzb_ref_kek_version, nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
     nzb_ref_payload_nonce, nzb_ref_payload_format)
VALUES ('11111111-2222-4333-8444-555555555555', 'original', decode('00', 'hex'),
        2, 1, decode(repeat('00', 12), 'hex'), decode(repeat('00', 48), 'hex'),
        decode(repeat('00', 12), 'hex'), 1);
"""
    cases = []
    for fence in ("open-fence", "deleting-fence", "missing-fence"):
        for expected in ("original", "stale"):
            switched = fence == "open-fence" and expected == "original"
            job = "replacement" if switched else "original"
            marker = "01" if switched else "00"
            cases.append(reset + f"""
EXECUTE switch_source (
    '11111111-2222-4333-8444-555555555555', '{expected}', 'replacement',
    decode('01', 'hex'), 2, decode(repeat('01', 12), 'hex'),
    decode(repeat('01', 48), 'hex'), decode(repeat('01', 12), 'hex'), 1, '{fence}');
DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM track_sources
                   WHERE source_job_id = '{job}'
                     AND encode(nzb_ref_encrypted, 'hex') = '{marker}'
                     AND nzb_ref_kek_version = {2 if switched else 1}
                     AND encode(nzb_ref_dek_wrapped_nonce, 'hex') = repeat('{marker}', 12)
                     AND encode(nzb_ref_dek_wrapped, 'hex') = repeat('{marker}', 48)
                     AND encode(nzb_ref_payload_nonce, 'hex') = repeat('{marker}', 12)) THEN
        RAISE EXCEPTION 'switch CAS/fence case failed';
    END IF;
END $$;
""")
    result = subprocess.run(
        ["psql", database_url, "-X", "-q", "-v", "ON_ERROR_STOP=1"],
        input=setup + "".join(cases) + "ROLLBACK;\n",
        text=True,
        capture_output=True,
        env={**os.environ, "PGOPTIONS": "-c search_path=pg_temp,public,pg_catalog"},
        timeout=30,
    )
    if result.returncode:
        raise AssertionError(result.stderr)
    print("pointer-switch migrated-schema CAS/fence cases passed: 6")


if __name__ == "__main__":
    main()
