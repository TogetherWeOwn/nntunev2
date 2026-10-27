#!/usr/bin/env python3
"""Builder-owned offline regression fixtures for the staging deployment lane."""

import copy
import io
import json
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest
from unittest.mock import patch
import urllib.error

import alerts_staging as guard

ACCOUNT = "a" * 32
LOGS = guard.logs_url(ACCOUNT)


def fixtures():
    settings = {
        "observability": {"enabled": False, "logs": {"enabled": False, "invocation_logs": False},
                          "traces": {"enabled": False}},
        "compatibility_date": "2026-10-01", "tail_consumers": [],
        "bindings": [
            {"name": "PRODUCER_SCRIPT_NAME", "type": "plain_text", "text": guard.PRODUCER},
            {"name": "LOGS_URL", "type": "plain_text", "text": LOGS},
            {"name": "ALERT_WINDOWS", "type": "durable_object_namespace",
             "class_name": "ErrorRateAlerts", "namespace_id": "namespace-id"},
        ],
    }
    namespaces = [{"script": guard.WORKER, "class": "ErrorRateAlerts", "id": "namespace-id"}]
    return settings, namespaces, {"enabled": False}, {"items": [{"id": "version-id"}]}


class VerificationTests(unittest.TestCase):
    def verify(self, data):
        return guard.verify(*data, LOGS)

    def test_unprovisioned_and_provisioned_secret_are_both_accepted(self):
        for secret in (False, True):
            with self.subTest(provisioned=secret):
                data = fixtures()
                if secret:
                    data[0]["bindings"].append({"name": guard.WEBHOOK, "type": "secret_text"})
                result = self.verify(data)
                self.assertEqual(result["version_id"], "version-id")
                self.assertEqual(result["do_namespace_id"], "namespace-id")
                self.assertEqual(guard.WEBHOOK in result["bindings"], secret)
                self.assertNotIn(LOGS, json.dumps(result))

    def test_secret_must_be_secret_text_and_extras_are_rejected(self):
        for binding, code in (
            ({"name": guard.WEBHOOK, "type": "plain_text", "text": "private-canary"},
             "WEBHOOK_BINDING_TYPE"),
            ({"name": "OTHER_SECRET", "type": "secret_text"}, "BINDING_DRIFT"),
        ):
            data = fixtures()
            data[0]["bindings"].append(binding)
            with self.subTest(code=code), self.assertRaisesRegex(guard.GuardError, code) as raised:
                self.verify(data)
            self.assertNotIn("private-canary", str(raised.exception))

    def test_each_observability_guard_rejects_on_or_missing(self):
        for section, key, code in (
            (None, "enabled", "OBSERVABILITY_NOT_OFF"),
            ("logs", "enabled", "LOGS_NOT_OFF"),
            ("logs", "invocation_logs", "INVOCATION_LOGS_NOT_OFF"),
            ("traces", "enabled", "TRACES_NOT_OFF"),
        ):
            for value in (True, None, 0):
                with self.subTest(section=section, key=key, value=value):
                    data = fixtures()
                    target = data[0]["observability"]
                    if section:
                        target = target[section]
                    target[key] = value
                    with self.assertRaisesRegex(guard.GuardError, code):
                        self.verify(data)
        for section in ("logs", "traces"):
            data = fixtures()
            data[0]["observability"][section] = None
            with self.subTest(section=section), self.assertRaises(guard.GuardError):
                self.verify(data)

    def test_compatibility_date_accepts_api_timestamp_and_rejects_drift(self):
        for value in ("2026-10-01", "2026-10-01T00:00:00Z"):
            data = fixtures()
            data[0]["compatibility_date"] = value
            self.verify(data)
        for value in (None, "", "2026-10-02", "2026-10-02T00:00:00Z", 20261001):
            data = fixtures()
            data[0]["compatibility_date"] = value
            with self.subTest(value=value), self.assertRaisesRegex(guard.GuardError, "COMPATIBILITY_DATE"):
                self.verify(data)

    def test_tail_consumers_are_absent_or_empty(self):
        for value in (None, []):
            data = fixtures()
            data[0]["tail_consumers"] = value
            self.verify(data)
        data = fixtures()
        data[0]["tail_consumers"] = [{"service": "unexpected-consumer"}]
        with self.assertRaisesRegex(guard.GuardError, "UNEXPECTED_TAIL_CONSUMERS"):
            self.verify(data)

    def test_binding_rejections(self):
        cases = (
            (0, "type", "secret_text", "PUBLIC_BINDING_TYPE"),
            (0, "text", "nntune-api", "PUBLIC_BINDING_VALUE"),
            (1, "type", "secret_text", "PUBLIC_BINDING_TYPE"),
            (1, "text", "https://example.invalid/logs", "PUBLIC_BINDING_VALUE"),
            (2, "type", "plain_text", "DO_BINDING_TYPE"),
            (2, "class_name", "OtherClass", "DO_BINDING_CLASS"),
            (2, "namespace_id", None, "DO_BINDING_NAMESPACE_MISSING"),
            (2, "namespace_id", "other-id", "DO_NAMESPACE_ID"),
        )
        for index, key, value, code in cases:
            data = fixtures()
            data[0]["bindings"][index][key] = value
            with self.subTest(key=key, code=code), self.assertRaisesRegex(guard.GuardError, code):
                self.verify(data)
        for index in range(3):
            data = fixtures()
            data[0]["bindings"].pop(index)
            with self.subTest(missing=index), self.assertRaisesRegex(guard.GuardError, "BINDING_DRIFT"):
                self.verify(data)
        data = fixtures()
        data[0]["bindings"].append(copy.deepcopy(data[0]["bindings"][0]))
        with self.assertRaisesRegex(guard.GuardError, "DUPLICATE_BINDINGS"):
            self.verify(data)

    def test_namespace_rejections(self):
        for entries, code in (
            ([], "DO_NAMESPACE_COUNT"),
            ([{"script": "nntune-alerts", "class": "ErrorRateAlerts", "id": "namespace-id"}],
             "DO_NAMESPACE_COUNT"),
            (fixtures()[1] * 2, "DO_NAMESPACE_COUNT"),
            ([{"script": guard.WORKER, "class": "OtherClass", "id": "namespace-id"}],
             "DO_NAMESPACE_CLASS"),
            ([{"script": guard.WORKER, "class": "ErrorRateAlerts", "id": "other-id"}],
             "DO_NAMESPACE_ID"),
        ):
            data = list(fixtures())
            data[1] = entries
            with self.subTest(code=code), self.assertRaisesRegex(guard.GuardError, code):
                self.verify(data)

    def test_subdomain_rejects_enabled_and_absent(self):
        for value in (True, None, 0):
            data = fixtures()
            data[2]["enabled"] = value
            with self.subTest(value=value), self.assertRaisesRegex(guard.GuardError, "WORKERS_DEV_NOT_OFF"):
                self.verify(data)

    def test_versions_shapes_and_missing_evidence(self):
        for versions in ({"items": [{"id": "version-id"}]}, [{"id": "version-id"}]):
            data = list(fixtures())
            data[3] = versions
            self.verify(data)
        for versions in (None, {}, [], {"items": []}, {"items": [{}]}):
            data = list(fixtures())
            data[3] = versions
            with self.subTest(versions=versions), self.assertRaisesRegex(guard.GuardError, "VERSION_EVIDENCE"):
                self.verify(data)


class PaginationAndTransportTests(unittest.TestCase):
    def test_namespace_on_second_page_is_verified(self):
        requested = []
        def get(path):
            requested.append(path)
            page = len(requested)
            return {"result": [{"script": "unrelated"}] if page == 1 else fixtures()[1],
                    "result_info": {"page": page, "total_pages": 2}}
        namespaces = guard.list_namespaces(get)
        self.assertEqual(requested, [
            "durable_objects/namespaces?per_page=1000&page=1",
            "durable_objects/namespaces?per_page=1000&page=2",
        ])
        data = list(fixtures())
        data[1] = namespaces
        guard.verify(*data, LOGS)

    def test_pagination_shape_and_page_mismatch_fail_closed(self):
        for info in ({}, {"page": 1, "total_pages": 0}, {"page": 1, "total_pages": True},
                     {"page": 1, "total_pages": 101}, {"page": 1, "total_pages": "2"},
                     {"page": 2, "total_pages": 2}):
            with self.subTest(info=info), self.assertRaises(guard.GuardError):
                guard.list_namespaces(lambda _: {"result": [], "result_info": info})
        with self.assertRaisesRegex(guard.GuardError, "NAMESPACE_LIST_SHAPE"):
            guard.list_namespaces(lambda _: {"result": {}})

    def test_transport_uses_get_and_rejects_api_failure_without_payload(self):
        for payload, code in (({"success": False, "errors": ["private-canary"]}, "API_UNSUCCESSFUL"),
                              ({"success": True}, "API_RESULT_MISSING"),
                              ([], "API_UNSUCCESSFUL")):
            with patch.object(guard.urllib.request, "build_opener") as opener:
                response = opener.return_value.open.return_value.__enter__.return_value
                response.read.return_value = json.dumps(payload).encode()
                with self.assertRaisesRegex(guard.GuardError, code) as raised:
                    guard.api_get(f"scripts/{guard.WORKER}/settings", ACCOUNT, "test-token")
                self.assertNotIn("private-canary", str(raised.exception))
                request = opener.return_value.open.call_args.args[0]
                self.assertEqual(request.method, "GET")
                self.assertIsNone(request.data)
                self.assertIsInstance(opener.call_args.args[0], guard.NoRedirect)
        with self.assertRaisesRegex(guard.GuardError, "API_TOKEN_MISSING"):
            guard.api_get("unused", ACCOUNT, "")
        self.assertIsNone(guard.NoRedirect().redirect_request(None, None, 302, None, {}, "https://example.invalid"))

    def test_transport_rejects_oversized_responses(self):
        with patch.object(guard.urllib.request, "build_opener") as opener:
            response = opener.return_value.open.return_value.__enter__.return_value
            response.read.return_value = b"x" * (8 * 1024 * 1024 + 1)
            with self.assertRaisesRegex(guard.GuardError, "API_RESPONSE_TOO_LARGE"):
                guard.api_get("unused", ACCOUNT, "test-token")
            response.read.assert_called_once_with(8 * 1024 * 1024 + 1)

    def test_transport_errors_and_invalid_json_are_redacted(self):
        for error in (urllib.error.URLError("private-canary"), TimeoutError("private-canary")):
            with patch.object(guard.urllib.request, "build_opener") as opener:
                opener.return_value.open.side_effect = error
                with self.assertRaisesRegex(guard.GuardError, "API_GET_FAILED") as raised:
                    guard.api_get("unused", ACCOUNT, "test-token")
                self.assertNotIn("private-canary", str(raised.exception))
        with patch.object(guard.urllib.request, "build_opener") as opener:
            opener.return_value.open.return_value.__enter__.return_value.read.return_value = b"private-canary"
            with self.assertRaisesRegex(guard.GuardError, "API_JSON_INVALID"):
                guard.api_get("unused", ACCOUNT, "test-token")


class ConfigAndFreezeTests(unittest.TestCase):
    def test_logs_url_rejections(self):
        guard.validate_logs_url(LOGS)
        for url in ("", None, "https://dash.cloudflare.com/" + "x" * 1024,
                    LOGS.replace("https:", "http:"), LOGS.replace("dash.", "user:pass@dash."),
                    LOGS.replace("dash.cloudflare.com", "dash.cloudflare.com:443"),
                    LOGS.replace("dash.cloudflare.com", "discord.com"),
                    LOGS.replace("/workers/services/view/", "/api/webhooks/"),
                    LOGS.replace("staging", "production"), LOGS + "?token=private-canary",
                    LOGS + "#fragment", LOGS + "\n", "https://[invalid"):
            with self.subTest(url=url), self.assertRaises(guard.GuardError) as raised:
                guard.validate_logs_url(url)
            self.assertNotIn("private-canary", str(raised.exception))
        for account in ("", "not-an-account", ACCOUNT + '"\n', None):
            with self.assertRaisesRegex(guard.GuardError, "ACCOUNT_ID_SHAPE"):
                guard.logs_url(account)

    def test_generated_config_is_staging_only_with_no_secret_or_routes(self):
        config = tomllib.loads(guard.generate_config(ACCOUNT))
        self.assertEqual(config["name"], guard.WORKER)
        self.assertFalse(config["workers_dev"])
        self.assertFalse(config["preview_urls"])
        self.assertFalse(config["observability"]["enabled"])
        self.assertEqual(set(config["vars"]), {"PRODUCER_SCRIPT_NAME", "LOGS_URL"})
        self.assertNotIn("routes", config)
        self.assertNotIn("tail_consumers", config)
        self.assertEqual(config["migrations"], [{"tag": "v1", "new_sqlite_classes": ["ErrorRateAlerts"]}])
        example = (guard.ROOT / "workers/observability/wrangler.toml.example").read_text()
        for text in (example.replace("enabled = false", "enabled = true", 1),
                     example.replace('name = "nntune-alerts-local"', 'name = "nntune-alerts"'),
                     example.replace('tag = "v1"', 'tag = "v2"'),
                     example.replace("workers_dev = false", "workers_dev = true"),
                     example + '\n[[routes]]\npattern = "example.invalid/*"\n',
                     example.replace('[vars]', '[vars]\nOPS_ALERT_WEBHOOK_URL = "private-canary"')):
            with self.subTest(drift=True), self.assertRaisesRegex(guard.GuardError, "CONFIG_DRIFT"):
                guard.generate_config(ACCOUNT, text)

    def test_frozen_guard_rejects_each_changed_or_missing_module_and_missing_commit(self):
        with tempfile.TemporaryDirectory(prefix="alerts-freeze-fixture-") as directory:
            repo = Path(directory)
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=repo, stderr=subprocess.DEVNULL).decode().strip()
            git("init", "-q")
            git("config", "user.name", "Fixture")
            git("config", "user.email", "fixture@example.invalid")
            for module in guard.MODULES:
                path = repo / module
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("// frozen fixture\n")
            git("add", ".")
            git("commit", "-qm", "fixture\n\nCo-Authored-By: Paperclip <noreply@paperclip.ing>")
            frozen = git("rev-parse", "HEAD")
            guard.check_frozen(repo, frozen)
            for module in guard.MODULES:
                path = repo / module
                for deleted in (False, True):
                    if deleted:
                        path.unlink()
                    else:
                        path.write_text("// changed fixture\n")
                    with self.subTest(module=module, deleted=deleted), self.assertRaisesRegex(
                        guard.GuardError, "FROZEN_RUNTIME_MISMATCH"
                    ):
                        guard.check_frozen(repo, frozen)
                    path.write_text("// frozen fixture\n")
            with self.assertRaisesRegex(guard.GuardError, "FROZEN_RUNTIME_MISMATCH"):
                guard.check_frozen(repo, "0" * 40)

    def test_ci_and_deploy_use_the_tested_entrypoints(self):
        workflow = (guard.ROOT / ".github/workflows/deploy-alerts-staging.yml").read_text()
        ci = (guard.ROOT / ".github/workflows/ci.yml").read_text()
        for command in ("freeze", "config", "verify"):
            self.assertIn(f"python3 .github/scripts/alerts_staging.py {command}", workflow)
        self.assertIn("python3 .github/scripts/test_alerts_staging.py", ci)
        self.assertIn("python3 .github/scripts/alerts_staging.py freeze", ci)
        self.assertIn(".github/scripts/alerts_staging.py", workflow.split("permissions:")[0])

    def test_cli_invalid_config_returns_sanitized_failure(self):
        with patch.dict(guard.os.environ, {"CLOUDFLARE_ACCOUNT_ID": "private-canary"}), \
             patch.object(guard.sys, "argv", ["alerts_staging.py", "config"]), \
             patch("sys.stdout", new_callable=io.StringIO) as output:
            self.assertEqual(guard.main(), 1)
        result = json.loads(output.getvalue())
        self.assertFalse(result["ok"])
        self.assertEqual(result["code"], "ACCOUNT_ID_SHAPE")
        self.assertNotIn("private-canary", output.getvalue())


if __name__ == "__main__":
    unittest.main()
