"""Shared validation for the web release adapters; no version allocation."""

from datetime import date
import hashlib
import json
from pathlib import PurePosixPath
import re


JOBS = {
    "staging-web": ["Deploy to staging"],
    "post-deploy-e2e / synthetic smoke (health)": ["Run synthetic smoke"],
    "post-deploy-e2e / full journey (signup → byop → ingest → meter → limits)": [
        "Run full journey", "Enforce journey evidence gate (all-skipped must fail)",
    ],
}
MAX_BYTES = 256 * 1024 * 1024
MAX_FILES = 20000


class Refusal(ValueError):
    pass


def require(condition, code):
    if not condition:
        raise Refusal(code)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False).encode()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def positive(value):
    return type(value) is int and 0 < value <= 9007199254740991


def release_version(value, canary=False):
    match = re.fullmatch(r"([1-9][0-9]{3})\.([1-9][0-9]{2,3})\.(0|[1-9][0-9]*)(?:-canary\.(0|[1-9][0-9]*))?", value or "")
    require(match is not None, "release-format")
    year, slot, patch, counter = match.groups()
    require((counter is not None) == canary, "release-channel")
    require(int(patch) <= 9007199254740991 and (counter is None or int(counter) <= 9007199254740991), "release-counter")
    parsed = date(int(year), int(slot[:-2]), int(slot[-2:]))
    require(str(parsed.month) + f"{parsed.day:02d}" == slot, "release-date")
    return parsed.isoformat()


def source_identity(repository, sha, run_id, attempt):
    require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository or "") is not None, "repository")
    require(re.fullmatch(r"[0-9a-f]{40}", sha or "") is not None, "source-sha")
    require(positive(run_id) and positive(attempt), "source-run")
    return {"repository": repository, "sha": sha, "runId": run_id, "attempt": attempt}


def validate_plan(plan, source):
    require(isinstance(plan, dict), "plan-shape")
    require(plan.get("schema") == "calver-plan-v1", "plan-schema")
    require(plan.get("repository") == source["repository"] and plan.get("sha") == source["sha"], "plan-source")
    selector = plan.get("source")
    require(isinstance(selector, dict), "plan-selector")
    require(selector.get("runId") == source["runId"] and selector.get("attempt") == source["attempt"], "plan-attempt")
    require(selector.get("workflow") == ".github/workflows/deploy.yml" and selector.get("jobs") == list(JOBS), "plan-policy")
    require(plan.get("policy") == "staged-promotion" and plan.get("channel") == "canary" and plan.get("namespace") == "", "plan-policy")
    require(plan.get("evidence") is None and plan.get("sourceCanary") is None and plan.get("repairDate") is None, "plan-deferred-source")
    require(isinstance(plan.get("operation"), str) and 0 < len(plan["operation"]) <= 256, "plan-operation")
    require(release_version(plan.get("version"), canary=True) == plan.get("date"), "plan-date")
    require(plan.get("tag") == "canary/v" + plan["version"], "plan-tag")
    payload = {key: value for key, value in plan.items() if key != "id"}
    require(plan.get("id") == digest(canonical(payload)), "plan-integrity")


def safe_path(value):
    require(isinstance(value, str) and bool(value), "member-path")
    require(not any(ord(c) < 32 or ord(c) == 127 for c in value), "member-path")
    require("\\" not in value and not value.startswith("/") and ":" not in value, "member-path")
    require(all(part not in ("", ".", "..") for part in value.split("/")), "member-path")
    require(PurePosixPath(value).as_posix() == value, "member-path")
    return value


def validate_manifest(manifest, source=None, plan_id=None, portable=False):
    require(isinstance(manifest, dict) and manifest.get("schema") == "nntune-web-release-v1", "manifest-schema")
    require(manifest.get("components") == ["web"], "manifest-components")
    actual = manifest.get("source", {})
    identity = source_identity(actual.get("repository"), actual.get("sha"), actual.get("runId"), actual.get("attempt"))
    require(source is None or identity == source, "manifest-source")
    validate_plan(manifest.get("plan"), identity)
    require(plan_id is None or manifest["plan"]["id"] == plan_id, "manifest-plan")
    compatibility = manifest.get("compatibility", {})
    require(isinstance(compatibility.get("date"), str), "compatibility-date")
    date.fromisoformat(compatibility["date"])
    require(isinstance(compatibility.get("flags"), list) and all(isinstance(f, str) for f in compatibility["flags"]), "compatibility-flags")
    build = manifest.get("build", {})
    require("publicApiBaseUrl" in build and (build["publicApiBaseUrl"] is None or isinstance(build["publicApiBaseUrl"], str)), "build-settings")
    require(not portable or build["publicApiBaseUrl"] is None, "nonportable-api-origin")
    files = manifest.get("files")
    require(isinstance(files, list) and 0 < len(files) <= MAX_FILES, "inventory-count")
    inventory = {}
    for entry in files:
        path = safe_path(entry.get("path"))
        require(path not in inventory, "inventory-duplicate")
        require(type(entry.get("size")) is int and 0 <= entry["size"] <= MAX_BYTES, "inventory-size")
        require(re.fullmatch(r"[0-9a-f]{64}", entry.get("sha256", "")) is not None, "inventory-hash")
        inventory[path] = entry
    require(sum(entry["size"] for entry in files) <= MAX_BYTES, "inventory-budget")
    require("_worker.js" in inventory or "_worker.js/index.js" in inventory, "worker-entrypoint")
    return inventory


def read_json(path):
    def unique(pairs):
        value = {}
        for key, item in pairs:
            require(key not in value, "json-duplicate")
            value[key] = item
        return value
    with open(path, encoding="utf-8") as handle:
        return json.load(handle, object_pairs_hook=unique)


def cli_result(action):
    try:
        result = action()
        print(json.dumps({"ok": True, **(result or {})}, ensure_ascii=False))
    except (ValueError, KeyError, TypeError, AttributeError, OSError, EOFError, RecursionError) as error:
        # Never echo file bodies, upstream responses or deployment values.
        code = str(error) if isinstance(error, Refusal) else "invalid-release-input"
        print(json.dumps({"ok": False, "code": code}))
        raise SystemExit(1)
