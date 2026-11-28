"""Compare the protected C2 response to the same deploy attempt and admitted SQL pins."""

import http.client
import json
import os
import re
import sys

from staging_api_identity import decode, require, Unavailable
from staging_trial_isolated import ROLES, SERVING, selfhost_target_config
from staging_trial_role_preflight import BINDINGS, DATABASE, binding_identity_verdicts, oid


def verify(mode, pins, deployment, reply):
    selected = BINDINGS[:1] if mode == "upload" else BINDINGS
    require(mode in ("upload", "all") and deployment["identity"] == "matched"
            and all(reply[key] == deployment[key] for key in ("version_id", "build_sha")), "deployment-mismatch")
    rows = [(binding, tuple(row)) for binding, row in reply["rows"]]
    roles = {binding: (ROLES[key], pins["role_oids"][key]) for binding, key in zip(BINDINGS, SERVING, strict=True)}
    db = pins["database_oid"]
    if mode == "upload":
        name, role_oid = roles[BINDINGS[0]]
        valid = rows == [(BINDINGS[0], (DATABASE, db, name, role_oid, name, role_oid))] and all(
            oid(rows[0][1][index]) for index in (1, 3, 5))
    else:
        valid = all(value == "matched" for value in binding_identity_verdicts(db, roles, rows).values())
    require(valid, "role-mismatch")
    return {key: deployment[key] for key in ("build_sha", "version_id", "deployment_id")} | {
        "bindings": dict.fromkeys(selected, "matched")}


if __name__ == "__main__":
    try:
        token = os.environ["PRIVATE_TRIAL_IDENTITY_TOKEN"]
        require(re.fullmatch(r"[A-Za-z0-9_-]{43}", token), "invalid-token")
        conn = http.client.HTTPSConnection("staging.nntune.com", timeout=20)
        try:
            conn.request("POST", "/v1/admin/private-trial/db-identity", body=b"",
                         headers={"Authorization": f"Bearer {token}", "Cache-Control": "no-store"})
            response = conn.getresponse()
            require(response.status == 200, "worker-status")
            data = response.read(4097)
            require(len(data) <= 4096, "worker-body-limit")
        finally:
            conn.close()
        result = verify(os.environ["PRIVATE_TRIAL_IDENTITY_MODE"], selfhost_target_config(os.environ["SELFHOST_TRIAL_PINS"]),
                        json.loads(os.environ["API_IDENTITY"]), decode(data, "worker-shape"))
    except (Unavailable, http.client.HTTPException, OSError, ValueError, KeyError, TypeError):
        sys.exit("Disarmed trial identity check refused.")
    print(json.dumps(result))
