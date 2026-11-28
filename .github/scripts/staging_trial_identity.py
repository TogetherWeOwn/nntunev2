"""Match the protected Worker reply to the deployed version and admitted SQL pins."""

import json
import os
import sys

from staging_api_identity import decode, require, Unavailable
from staging_trial_isolated import ROLES, SERVING, selfhost_target_config
from staging_trial_role_preflight import BINDINGS, DATABASE, oid


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
        print(json.dumps(verify(os.environ["PRIVATE_TRIAL_IDENTITY_MODE"], selfhost_target_config(os.environ["SELFHOST_TRIAL_PINS"]),
                                json.loads(os.environ["API_IDENTITY"]), decode(data, "worker-shape"))))
    except (Unavailable, ValueError, KeyError, TypeError):
        sys.exit("Disarmed trial identity check refused.")
