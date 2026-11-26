"""Compare saved Reader evidence with two synthetic inherited grant paths."""

import hashlib
import json
import os
import signal
import subprocess
import sys
import tempfile
from pathlib import Path

from test_database_bootstrap_isolation import (
    Cluster, FixtureFailure, check_interruption, checked, remember_signal, require,
)


def readback(cluster, granting_group):
    query = cluster.sql(f"""
        BEGIN;
        GRANT SELECT ON membership_fixture.required TO {granting_group};
        COMMIT;
        BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;
        SELECT d.oid, 'membership_reader'::regrole::oid, c.oid::regclass::text,
               has_table_privilege('membership_reader', c.oid, 'SELECT'),
               pg_has_role('membership_reader', 'membership_reviewed', 'USAGE'),
               (SELECT string_agg(a.grantee::regrole::text, ',' ORDER BY a.grantee)
                FROM aclexplode(c.relacl) a
                WHERE a.grantee <> c.relowner AND a.privilege_type = 'SELECT')
        FROM pg_database d, pg_class c
        WHERE d.datname = current_database() AND c.oid = 'membership_fixture.required'::regclass;
        COMMIT;
        REVOKE SELECT ON membership_fixture.required FROM {granting_group};
        """)
    checked(query, "read the synthetic inventory and grant in one snapshot")
    return query.stdout.strip().split("|")


def check_inherited_select(cluster):
    cluster.ok("prepare the synthetic membership comparison", """
        CREATE ROLE membership_reader NOLOGIN;
        CREATE ROLE membership_reviewed NOLOGIN;
        CREATE ROLE membership_alternate NOLOGIN;
        GRANT membership_reviewed, membership_alternate TO membership_reader WITH INHERIT TRUE;
        CREATE SCHEMA membership_fixture;
        CREATE TABLE membership_fixture.required (id integer);
        GRANT USAGE ON SCHEMA membership_fixture TO membership_reader;
        """)
    control = readback(cluster, "membership_reviewed")
    alternate = readback(cluster, "membership_alternate")
    require(control[2:] == ["membership_fixture.required", "t", "t", "membership_reviewed"],
            "verify effective SELECT and its reviewed grant in the control")
    require(alternate == control[:-1] + ["membership_alternate"],
            "verify equal effective SELECT with the reviewed grant absent")

    def evidence(snapshot):
        database_oid, reader_oid, relation, effective_select, _, _ = snapshot
        inventory = {"branch_id": cluster.branch_id, "database_oid": int(database_oid),
                     "inventory_sha256": hashlib.sha256(relation.encode("ascii")).hexdigest()}
        reader = inventory | {"reader_role": "membership_reader", "reader_oid": int(reader_oid)}
        return {"inventory": inventory, "reader_privileges": reader | {
            "result": "passed" if effective_select == "t" else "failed",
        }}

    control_evidence = evidence(control)
    alternate_evidence = evidence(alternate)
    review = {key: value for key, value in control_evidence["reader_privileges"].items() if key != "result"}
    checker = Path(__file__).with_name("database_bootstrap_reader_evidence.py")
    consistent = (0, b"database-bootstrap-reader-evidence: consistent\n", b"")
    pending = (1, b"", b"database-bootstrap-reader-evidence: pending\n")
    # Effective access alone produces the same receipt for both grant paths.
    # A review that requires the reviewed group has no matching privilege
    # receipt for the alternate observation, so it submits the inventory alone.
    for observed, expected in (
        (control_evidence, consistent),
        (alternate_evidence, consistent),
        ({"inventory": alternate_evidence["inventory"]}, pending),
    ):
        record = {"review": review, **observed}
        result = subprocess.run([sys.executable, "-IB", checker], input=json.dumps(record).encode(),
                                capture_output=True, timeout=5)
        require((result.returncode, result.stdout, result.stderr) == expected,
                "reconcile the saved privilege result against the reviewed evidence")


if __name__ == "__main__":
    os.umask(0o077)
    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, remember_signal)
    try:
        require(len(sys.argv) == 1, "accept no arguments")
        with tempfile.TemporaryDirectory(prefix="bootstrap-membership-") as directory:
            cluster = Cluster(Path(directory), "fixture001")
            try:
                cluster.start()
                check_inherited_select(cluster)
            finally:
                cluster.teardown()
        require(not Path(directory).exists(), "verify generated files were removed")
        check_interruption()
    except FixtureFailure as error:
        sys.exit(f"database-bootstrap-inherited-select: failed to {error}")
    except Exception:
        sys.exit("database-bootstrap-inherited-select: failed")
    print("database-bootstrap-inherited-select: catalog grant paths and offline receipt verdicts verified")
