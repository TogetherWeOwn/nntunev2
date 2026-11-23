"""Generated data for the disposable setup caller fixture."""

import base64
import hashlib
import json
import secrets

import database_bootstrap_admission as admission


def plan_binding(raw):
    return {
        "BOOTSTRAP_PLAN_B64": base64.b64encode(raw).decode(),
        "BOOTSTRAP_PLAN_DIGEST": hashlib.sha256(raw).hexdigest(),
    }


def caller_env(cluster, roles, **changes):
    setup, owner, migrator, reader = roles
    sha, run = secrets.token_hex(20), str(secrets.randbelow(10**10) + 1)
    plan = {
        "version": 2, "host": cluster.address, "port": 5432,
        "ca_pem": (cluster.directory / "client" / "trusted.crt").read_text(),
        "maintenance": "postgres", "launch": "launch_fixture", "setup_role": setup,
        "branch_id": cluster.branch_id, "setup_login": f"{setup}.{cluster.branch_id}",
        "owner_role": owner, "migrator_role": migrator, "reader_role": reader,
    }
    raw = json.dumps(plan | changes).encode()
    event = cluster.directory / "event.json"
    event.write_text("{}")
    return {
        "GITHUB_REPOSITORY": admission.REPOSITORY,
        "GITHUB_WORKFLOW_REF": f"{admission.REPOSITORY}/{admission.WORKFLOW}@{admission.MAIN}",
        "GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_REF": admission.MAIN,
        "GITHUB_SHA": sha, "GITHUB_WORKFLOW_SHA": sha, "GITHUB_RUN_ID": run,
        "GITHUB_RUN_ATTEMPT": "1", "GITHUB_EVENT_PATH": str(event),
        "BOOTSTRAP_SHA": sha, "BOOTSTRAP_RUN_ID": run, "BOOTSTRAP_EXPIRY": str(cluster.expiry),
        **plan_binding(raw),
        "BOOTSTRAP_SETUP_PASSWORD": cluster.passwords[setup], "PATH": "/usr/bin:/bin",
        "TMPDIR": str(cluster.directory),
    }
