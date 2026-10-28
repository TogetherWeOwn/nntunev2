#!/usr/bin/env python3
"""Frozen staging alert deployment guards. Verification uses GET requests only."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
FROZEN_SHA = "818e57680414c4414cc3c96e2f3f9a6ca38d80bf"
MODULES = tuple(
    f"workers/observability/{name}.mjs"
    for name in ("runtime", "project-tail-event", "evaluate-error-window", "render-verdict-alert")
)
WORKER = "nntune-alerts-staging"
PRODUCER = "nntune-api-staging"
COMPATIBILITY_DATE = "2026-10-01"
REQUIRED_BINDINGS = {"PRODUCER_SCRIPT_NAME", "LOGS_URL", "ALERT_WINDOWS"}
WEBHOOK = "OPS_ALERT_WEBHOOK_URL"


class GuardError(ValueError):
    """A static diagnostic, never API payloads, binding values or credentials."""


def require(condition, code):
    if not condition:
        raise GuardError(code)


def check_frozen(repo=ROOT, frozen=FROZEN_SHA):
    for args in (("cat-file", "-e", f"{frozen}^{{commit}}"),
                 ("diff", "--quiet", frozen, "--", *MODULES)):
        result = subprocess.run(
            ["git", *args], cwd=repo, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            check=False,
        )
        require(result.returncode == 0, "FROZEN_RUNTIME_MISMATCH")


def validate_logs_url(url):
    require(isinstance(url, str) and 0 < len(url) <= 1024, "LOGS_URL_LENGTH")
    require(not any(c.isspace() or ord(c) < 32 for c in url), "LOGS_URL_WHITESPACE")
    try:
        parts = urllib.parse.urlsplit(url)
        require(parts.scheme == "https", "LOGS_URL_SCHEME")
        require(parts.netloc == "dash.cloudflare.com", "LOGS_URL_HOST")
        require(not parts.query and not parts.fragment, "LOGS_URL_SUFFIX")
        require(re.fullmatch(
            rf"/[0-9a-fA-F]{{32}}/workers/services/view/{PRODUCER}/observability/logs",
            parts.path,
        ), "LOGS_URL_PATH")
    except ValueError as error:
        if isinstance(error, GuardError):
            raise
        raise GuardError("LOGS_URL_PARSE") from None


def logs_url(account_id):
    require(isinstance(account_id, str) and re.fullmatch(r"[0-9a-fA-F]{32}", account_id),
            "ACCOUNT_ID_SHAPE")
    url = f"https://dash.cloudflare.com/{account_id}/workers/services/view/{PRODUCER}/observability/logs"
    validate_logs_url(url)
    return url


def generate_config(account_id, example=None):
    expected_logs = logs_url(account_id)
    if example is None:
        example = (ROOT / "workers/observability/wrangler.toml.example").read_text()
    text = example.replace('name = "nntune-alerts-local"', f'name = "{WORKER}"', 1)
    text = text.replace("https://logs.example.invalid/unconfigured", expected_logs, 1)
    expected = {
        "name": WORKER, "main": "runtime.mjs", "compatibility_date": COMPATIBILITY_DATE,
        "workers_dev": False, "preview_urls": False, "send_metrics": False,
        "observability": {
            "enabled": False, "logs": {"enabled": False, "invocation_logs": False},
            "traces": {"enabled": False},
        },
        "vars": {"PRODUCER_SCRIPT_NAME": PRODUCER, "LOGS_URL": expected_logs},
        "durable_objects": {"bindings": [{"name": "ALERT_WINDOWS", "class_name": "ErrorRateAlerts"}]},
        "migrations": [{"tag": "v1", "new_sqlite_classes": ["ErrorRateAlerts"]}],
    }
    require(tomllib.loads(text) == expected, "CONFIG_DRIFT")
    return text


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def api_get(path, account_id, token):
    logs_url(account_id)
    require(bool(token), "API_TOKEN_MISSING")
    request = urllib.request.Request(
        f"https://api.cloudflare.com/client/v4/accounts/{account_id}/workers/{path}",
        headers={"Authorization": f"Bearer {token}"}, method="GET",
    )
    try:
        with urllib.request.build_opener(NoRedirect()).open(request, timeout=20) as response:
            raw = response.read(8 * 1024 * 1024 + 1)
        require(len(raw) <= 8 * 1024 * 1024, "API_RESPONSE_TOO_LARGE")
        payload = json.loads(raw)
    except GuardError:
        raise
    except (urllib.error.URLError, TimeoutError, OSError):
        raise GuardError("API_GET_FAILED") from None
    except (ValueError, UnicodeError):
        raise GuardError("API_JSON_INVALID") from None
    require(isinstance(payload, dict) and payload.get("success") is True, "API_UNSUCCESSFUL")
    require("result" in payload, "API_RESULT_MISSING")
    return payload


def list_namespaces(get):
    namespaces = []
    seen_ids = set()
    metadata = None
    per_page = 1000
    for page in range(1, 101):
        payload = get(f"durable_objects/namespaces?per_page={per_page}&page={page}")
        batch = payload["result"]
        require(isinstance(batch, list), "NAMESPACE_LIST_SHAPE")
        require(len(batch) <= per_page, "NAMESPACE_PAGE_SIZE")
        info = payload.get("result_info")
        require(isinstance(info, dict), "NAMESPACE_PAGES_SHAPE")
        require(type(info.get("page")) is int and info["page"] == page,
                "NAMESPACE_PAGE_MISMATCH")
        for key, minimum, maximum in (
            ("per_page", 1, per_page), ("count", 0, per_page),
            ("total_count", 0, per_page * 100), ("total_pages", 1, 100),
        ):
            if key in info:
                require(type(info[key]) is int and minimum <= info[key] <= maximum,
                        "NAMESPACE_PAGES_SHAPE")
        if "per_page" in info:
            require(info["per_page"] == per_page, "NAMESPACE_PAGE_SIZE")
        if "count" in info:
            require(info["count"] == len(batch), "NAMESPACE_COUNT_MISMATCH")

        # Do not let later pages change or drop the evidence used to enumerate.
        current = (info.get("per_page"), info.get("total_count"),
                   info.get("total_pages"), "count" in info)
        if metadata is None:
            metadata = current
        require(current == metadata, "NAMESPACE_METADATA_CHANGED")
        total_pages = info.get("total_pages")
        total_count = info.get("total_count")
        # Cloudflare documents total_pages as optional. Its alternative must be
        # explicit count evidence, never a missing-metadata single-page fallback.
        require(total_pages is not None or {"count", "per_page", "total_count"} <= info.keys(),
                "NAMESPACE_PAGES_SHAPE")
        if total_count is not None:
            counted_pages = max(1, (total_count + per_page - 1) // per_page)
            require(total_pages is None or total_pages == counted_pages,
                    "NAMESPACE_COUNT_MISMATCH")
            total_pages = counted_pages
            require(len(batch) == min(per_page, max(0, total_count - len(namespaces))),
                    "NAMESPACE_COUNT_MISMATCH")
        elif page < total_pages:
            require(len(batch) == per_page, "NAMESPACE_PAGE_SIZE")
        require(page <= total_pages, "NAMESPACE_PAGE_MISMATCH")
        require(bool(batch) or page == total_pages == 1, "NAMESPACE_EMPTY_PAGE")

        for namespace in batch:
            require(isinstance(namespace, dict), "NAMESPACE_ITEM_SHAPE")
            # Legacy explicit-page responses may omit IDs for unrelated entries.
            # Counted inventories need IDs so overlap cannot fake completeness.
            if total_count is not None or "id" in namespace:
                namespace_id = namespace.get("id")
                require(isinstance(namespace_id, str) and bool(namespace_id), "NAMESPACE_ID_SHAPE")
                require(namespace_id not in seen_ids, "NAMESPACE_DUPLICATE_ID")
                seen_ids.add(namespace_id)
        namespaces.extend(batch)
        if page == total_pages:
            return namespaces
    raise GuardError("NAMESPACE_PAGES_SHAPE")


def verify(settings, namespaces, subdomain, versions, expected_logs):
    validate_logs_url(expected_logs)
    obs = settings.get("observability") or {}
    require(obs.get("enabled") is False, "OBSERVABILITY_NOT_OFF")
    require((obs.get("logs") or {}).get("enabled") is False, "LOGS_NOT_OFF")
    require((obs.get("logs") or {}).get("invocation_logs") is False, "INVOCATION_LOGS_NOT_OFF")
    require((obs.get("traces") or {}).get("enabled") is False, "TRACES_NOT_OFF")
    date = settings.get("compatibility_date")
    require(isinstance(date, str) and date.split("T", 1)[0] == COMPATIBILITY_DATE,
            "COMPATIBILITY_DATE_DRIFT")
    require(settings.get("tail_consumers") in (None, []), "UNEXPECTED_TAIL_CONSUMERS")
    bindings = settings.get("bindings", [])
    by_name = {binding["name"]: binding for binding in bindings}
    require(len(by_name) == len(bindings), "DUPLICATE_BINDINGS")
    require(REQUIRED_BINDINGS <= set(by_name) <= REQUIRED_BINDINGS | {WEBHOOK}, "BINDING_DRIFT")
    for name, expected in (("PRODUCER_SCRIPT_NAME", PRODUCER), ("LOGS_URL", expected_logs)):
        require(by_name[name].get("type") == "plain_text", "PUBLIC_BINDING_TYPE")
        require(by_name[name].get("text") == expected, "PUBLIC_BINDING_VALUE")
    if WEBHOOK in by_name:
        require(by_name[WEBHOOK].get("type") == "secret_text", "WEBHOOK_BINDING_TYPE")
    binding = by_name["ALERT_WINDOWS"]
    require(binding.get("type") == "durable_object_namespace", "DO_BINDING_TYPE")
    require(binding.get("class_name") == "ErrorRateAlerts", "DO_BINDING_CLASS")
    require(bool(binding.get("namespace_id")), "DO_BINDING_NAMESPACE_MISSING")
    mine = [namespace for namespace in namespaces if namespace.get("script") == WORKER]
    require(len(mine) == 1, "DO_NAMESPACE_COUNT")
    require(mine[0].get("class") == "ErrorRateAlerts", "DO_NAMESPACE_CLASS")
    require(mine[0].get("id") == binding["namespace_id"], "DO_NAMESPACE_ID")
    require(subdomain.get("enabled") is False, "WORKERS_DEV_NOT_OFF")
    items = versions.get("items") if isinstance(versions, dict) else versions
    require(isinstance(items, list) and bool(items) and bool(items[0].get("id")),
            "VERSION_EVIDENCE_MISSING")
    return {"worker": WORKER, "version_id": items[0]["id"],
            "do_namespace_id": mine[0]["id"], "bindings": sorted(by_name)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("freeze", "config", "verify"))
    args = parser.parse_args()
    try:
        if args.command == "freeze":
            check_frozen()
            result = {"frozen_sha": FROZEN_SHA, "modules": len(MODULES)}
        elif args.command == "config":
            text = generate_config(os.environ.get("CLOUDFLARE_ACCOUNT_ID", ""))
            (ROOT / "workers/observability/wrangler.staging.toml").write_text(text)
            result = {"worker": WORKER, "config": "wrangler.staging.toml"}
        else:
            account_id = os.environ.get("CLOUDFLARE_ACCOUNT_ID", "")
            token = os.environ.get("CLOUDFLARE_API_TOKEN", "")
            def get(path):
                return api_get(path, account_id, token)
            result = verify(
                get(f"scripts/{WORKER}/settings")["result"], list_namespaces(get),
                get(f"scripts/{WORKER}/subdomain")["result"],
                get(f"scripts/{WORKER}/versions?per_page=1")["result"], logs_url(account_id),
            )
        print(json.dumps({"ok": True, **result}))
        return 0
    except (GuardError, OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        code = str(error) if isinstance(error, GuardError) else "INVALID_RESPONSE_OR_CONFIG"
        print(json.dumps({"ok": False, "code": code, "retryable": False,
                          "next_action": "Inspect the staging guard; do not bypass or retry deployment."}))
        return 1


if __name__ == "__main__":
    sys.exit(main())
