"""Match the protected Worker reply to the deployed version and admitted SQL pins."""

import json
import os
import re
import sys

from staging_api_identity import decode, require, Unavailable
from staging_trial_isolated import ROLES, SERVING, selfhost_target_config
from staging_trial_role_preflight import BINDINGS, DATABASE, oid


def failure_diagnostic(mode, deployment, reply):
    require(mode in ("upload", "all") and deployment["identity"] == "matched"
            and reply["build_sha"] == deployment["build_sha"], "deployment-mismatch")
    require(reply["code"] == "service_unavailable", "worker-shape")
    diagnostic = reply["diagnostic"]
    phase, binding = diagnostic["phase"], diagnostic["binding"]
    error_class, sqlstate = diagnostic["error_class"], diagnostic["sqlstate"]
    bindings = BINDINGS[:1] if mode == "upload" else BINDINGS
    require(
        (phase == "version_metadata" and binding is None)
        or (binding in bindings and phase in (
            "role_env", "hyperdrive_binding", "binding_identity", "edge_connect",
            "postgres_startup_auth", "role_query", "identity_begin", "identity_query",
            "identity_rollback", "identity_decode",
        )), "diagnostic-phase",
    )
    require(error_class in ("missing_or_invalid_binding", "identity_mismatch", "socket", "deadline",
                            "authentication", "postgres", "connection_closed", "driver", "invalid_identity"),
            "diagnostic-class")
    require(sqlstate is None or (isinstance(sqlstate, str) and re.fullmatch(r"[A-Z0-9]{5}", sqlstate)),
            "diagnostic-sqlstate")
    return dict(binding=binding, phase=phase, error_class=error_class, sqlstate=sqlstate)


def verify(mode, pins, deployment, reply):
    require(mode in ("upload", "all") and deployment["identity"] == "matched"
            and all(reply[key] == deployment[key] for key in ("version_id", "build_sha")), "deployment-mismatch")
    bindings = BINDINGS[:1] if mode == "upload" else BINDINGS
    expected = []
    for binding, key in zip(bindings, SERVING[:len(bindings)], strict=True):
        name, role_oid = ROLES[key], pins["role_oids"][key]
        expected.append([binding, [DATABASE, pins["database_oid"], name, role_oid, name, role_oid]])
    rows = reply["rows"]
    require(rows == expected and all(oid(row[i]) for _, row in rows for i in (1, 3, 5)), "role-mismatch")
    return {key: deployment[key] for key in ("build_sha", "version_id", "deployment_id")} | {
        "bindings": dict.fromkeys(bindings, "matched")}


if __name__ == "__main__":
    try:
        data = sys.stdin.buffer.read(4097)
        require(len(data) <= 4096, "worker-body-limit")
        mode = os.environ["PRIVATE_TRIAL_IDENTITY_MODE"]
        deployment = json.loads(os.environ["API_IDENTITY"])
        reply = decode(data, "worker-shape")
        if "diagnostic" in reply:
            diagnostic = failure_diagnostic(mode, deployment, reply)
            sys.exit(f"Disarmed trial identity check refused. {json.dumps(diagnostic)}")
        print(json.dumps(verify(mode, selfhost_target_config(os.environ["SELFHOST_TRIAL_PINS"]),
                                deployment, reply)))
    except (Unavailable, ValueError, KeyError, TypeError):
        sys.exit("Disarmed trial identity check refused.")
