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
from urllib.parse import parse_qs, urlsplit

import alerts_staging as guard
from test_ci_pnpm_retry import (
    check_required_step, check_step_shell, job_level_field, job_needs, non_plain_key_lines,
    split_jobs, split_steps, step_field, step_level_field,
)

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
            return {"result": [
                        {"script": "unrelated", "id": f"unrelated-{index}"}
                        for index in range(1000)
                    ] if page == 1 else fixtures()[1],
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

    def test_guard_identity_constants_are_literal(self):
        self.assertEqual(guard.WORKER, "nntune-alerts-staging")
        self.assertEqual(guard.PRODUCER, "nntune-api-staging")
        self.assertEqual(guard.COMPATIBILITY_DATE, "2026-10-01")
        self.assertEqual(guard.MODULES, (
            "workers/observability/runtime.mjs",
            "workers/observability/project-tail-event.mjs",
            "workers/observability/evaluate-error-window.mjs",
            "workers/observability/render-verdict-alert.mjs",
        ))

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


class IsolatedNamespaceGuardTests(unittest.TestCase):
    def setUp(self):
        network = patch("socket.socket", side_effect=AssertionError("Offline tests forbid network"))
        network.start()
        self.addCleanup(network.stop)

    def pages(self):
        # No count metadata: count-completeness checks must not mask the
        # explicit-page fullness, metadata-stability or empty-page guards.
        first = [*fixtures()[1], *[
            {"script": "unrelated", "class": "OtherClass", "id": f"unrelated-{index}"}
            for index in range(999)
        ]]
        return [
            {"result": first, "result_info": {"page": 1, "total_pages": 2}},
            {"result": [{"script": "unrelated", "class": "OtherClass", "id": "last-id"}],
             "result_info": {"page": 2, "total_pages": 2}},
        ]

    def run_cli(self, pages):
        settings, _, subdomain, versions = fixtures()
        routes = {
            "scripts/nntune-alerts-staging/settings": settings,
            "scripts/nntune-alerts-staging/subdomain": subdomain,
            "scripts/nntune-alerts-staging/versions?per_page=1": versions,
        }
        requested = []

        def get(path, account_id, token):
            self.assertEqual(account_id, ACCOUNT)
            self.assertEqual(token, "")
            parts = urlsplit(path)
            if parts.path == "durable_objects/namespaces":
                self.assertEqual((parts.scheme, parts.netloc, parts.fragment), ("", "", ""))
                query = parse_qs(parts.query, strict_parsing=True)
                self.assertEqual(query, {"per_page": ["1000"], "page": [str(len(requested) + 1)]})
                number = int(query["page"][0])
                requested.append(number)
                if number <= len(pages):
                    return copy.deepcopy(pages[number - 1])
                self.assertEqual(number, len(pages) + 1, "Unbounded pagination")
                return {"result": [], "result_info": {"page": number, "count": 0,
                        "per_page": 1000, "total_count": sum(len(p["result"]) for p in pages)}}
            self.assertIn(path, routes, "Unexpected offline verifier endpoint")
            return {"success": True, "result": copy.deepcopy(routes[path])}

        with patch.object(guard, "api_get", side_effect=get), \
             patch.object(guard.os, "environ", {"CLOUDFLARE_ACCOUNT_ID": ACCOUNT}), \
             patch.object(guard.sys, "argv", ["alerts_staging.py", "verify"]), \
             patch("sys.stdout", new_callable=io.StringIO) as output:
            status = guard.main()
        return status, json.loads(output.getvalue()), requested

    def assert_accepted(self, pages):
        status, report, requested = self.run_cli(pages)
        self.assertEqual(status, 0, report)
        self.assertEqual(report, {
            "ok": True, "worker": "nntune-alerts-staging", "version_id": "version-id",
            "do_namespace_id": "namespace-id",
            "bindings": ["ALERT_WINDOWS", "LOGS_URL", "PRODUCER_SCRIPT_NAME"],
        })
        self.assertIn(requested, (list(range(1, len(pages) + 1)),
                                  list(range(1, len(pages) + 2))))

    def assert_rejected(self, pages, code=None):
        status, report, requested = self.run_cli(pages)
        self.assertEqual(status, 1, f"Unsafe inventory was accepted: {report}")
        self.assertLessEqual(set(report), {"ok", "code", "retryable", "next_action"})
        self.assertIs(report["ok"], False)
        self.assertIs(report["retryable"], False)
        self.assertRegex(report["code"], r"^[A-Z][A-Z0-9_]+$")
        if code is not None:
            self.assertEqual(report["code"], code)
        self.assertNotIn("private-pagination-canary", json.dumps(report))
        self.assertNotIn("version_id", report)
        self.assertLessEqual(len(requested), len(pages) + 1)

    def test_full_explicit_pages_with_only_page_and_total_pages_succeed(self):
        self.assert_accepted(self.pages())

    def test_short_explicit_single_page_succeeds(self):
        self.assert_accepted([{"result": fixtures()[1],
                               "result_info": {"page": 1, "total_pages": 1}}])

    def test_count_complete_pages_and_valid_unrelated_ids_succeed(self):
        pages = self.pages()
        for page in pages:
            page["result_info"].pop("total_pages")
            page["result_info"].update(count=len(page["result"]), per_page=1000, total_count=1001)
        self.assert_accepted(pages)

    def test_short_nonterminal_page_without_optional_counts_is_rejected(self):
        for target_first in (False, True):
            with self.subTest(target_first=target_first):
                pages = self.pages()
                pages[0]["result"] = fixtures()[1] if target_first else [pages[0]["result"][1]]
                pages[1]["result"] = [pages[1]["result"][0]] if target_first else fixtures()[1]
                pages[0]["result"][0]["name"] = "private-pagination-canary"
                self.assert_rejected(pages)

    def test_short_nonterminal_page_with_only_per_page_is_rejected(self):
        pages = self.pages()
        pages[0]["result"] = fixtures()[1]
        for page in pages:
            page["result_info"]["per_page"] = 1000
        self.assert_rejected(pages)

    def test_empty_terminal_explicit_page_without_total_count_is_rejected(self):
        pages = self.pages()
        pages[1]["result"] = []
        self.assert_rejected(pages)

    def test_individually_valid_changed_completion_metadata_is_rejected(self):
        pages = self.pages()
        pages[0]["result_info"]["total_pages"] = 3
        self.assert_rejected(pages)

    def test_later_page_cannot_drop_completion_evidence(self):
        pages = self.pages()
        pages[1]["result_info"] = {"page": 2}
        self.assert_rejected(pages)

    def test_optional_metadata_used_on_the_first_page_cannot_disappear(self):
        for key, value in (("per_page", 1000), ("count", 1000), ("total_count", 1001)):
            with self.subTest(key=key):
                pages = self.pages()
                pages[0]["result_info"][key] = value
                self.assert_rejected(pages)
        pages = self.pages()
        for page in pages:
            page["result_info"].update(count=len(page["result"]), per_page=1000, total_count=1001)
        del pages[1]["result_info"]["total_pages"]
        self.assert_rejected(pages)

    def test_later_page_cannot_drop_counted_completion_evidence(self):
        pages = self.pages()
        pages[0]["result_info"] = {"page": 1, "count": 1000, "per_page": 1000, "total_count": 1001}
        pages[1]["result_info"] = {"page": 2}
        self.assert_rejected(pages)

    def test_unrelated_counted_rows_require_nonempty_string_ids(self):
        for missing, value in ((True, None), (False, None), (False, ""),
                               (False, 7), (False, True)):
            with self.subTest(missing=missing, value=value):
                unrelated = {"script": "unrelated", "class": "OtherClass",
                             "name": "private-pagination-canary"}
                if not missing:
                    unrelated["id"] = value
                self.assert_rejected([{
                    "result": [*fixtures()[1], unrelated],
                    "result_info": {"page": 1, "count": 2, "per_page": 1000, "total_count": 2},
                }])

    def test_short_nonempty_counted_terminal_page_is_rejected(self):
        for explicit_pages in (False, True):
            for target_first in (False, True):
                with self.subTest(explicit_pages=explicit_pages, target_first=target_first):
                    pages = self.pages()
                    if not target_first:
                        pages[0]["result"][0], pages[1]["result"][0] = (
                            pages[1]["result"][0], pages[0]["result"][0]
                        )
                    for page in pages:
                        if not explicit_pages:
                            page["result_info"].pop("total_pages")
                        page["result_info"].update(
                            count=len(page["result"]), per_page=1000, total_count=1005
                        )
                        page["result"][0]["name"] = "private-pagination-canary"
                    self.assert_rejected(pages, "NAMESPACE_COUNT_MISMATCH")

    def test_explicit_only_per_page_must_match_the_request(self):
        target = copy.deepcopy(fixtures()[1])
        target[0]["name"] = "private-pagination-canary"
        self.assert_rejected([{
            "result": target,
            "result_info": {"page": 1, "total_pages": 1, "per_page": 999},
        }], "NAMESPACE_PAGE_SIZE")

    def test_explicit_only_count_must_match_the_batch(self):
        target = copy.deepcopy(fixtures()[1])
        target[0]["name"] = "private-pagination-canary"
        self.assert_rejected([{
            "result": target,
            "result_info": {"page": 1, "total_pages": 1, "count": 0},
        }], "NAMESPACE_COUNT_MISMATCH")

    def test_present_explicit_only_ids_must_be_nonempty_strings(self):
        for value in (None, "", 7, True, [], {}):
            with self.subTest(value=value):
                self.assert_rejected([{
                    "result": [*fixtures()[1], {
                        "script": "unrelated", "class": "OtherClass", "id": value,
                        "name": "private-pagination-canary",
                    }],
                    "result_info": {"page": 1, "total_pages": 1},
                }], "NAMESPACE_ID_SHAPE")

    def test_duplicate_present_explicit_only_ids_are_rejected(self):
        unrelated = {"script": "unrelated", "class": "OtherClass", "id": "same-id",
                     "name": "private-pagination-canary"}
        self.assert_rejected([{
            "result": [*fixtures()[1], unrelated, copy.deepcopy(unrelated)],
            "result_info": {"page": 1, "total_pages": 1},
        }], "NAMESPACE_DUPLICATE_ID")

    def test_duplicate_counted_ids_on_the_first_page_are_rejected(self):
        unrelated = {"script": "unrelated", "class": "OtherClass", "id": "same-id",
                     "name": "private-pagination-canary"}
        self.assert_rejected([{
            "result": [*fixtures()[1], unrelated, copy.deepcopy(unrelated)],
            "result_info": {"page": 1, "count": 3, "per_page": 1000, "total_count": 3},
        }], "NAMESPACE_DUPLICATE_ID")

    def test_oversized_explicit_only_terminal_page_is_rejected(self):
        # Omit optional count/per_page: their own limits must not mask batch size.
        self.assert_rejected([{
            "result": [*fixtures()[1], *[
                {"script": "unrelated", "class": "OtherClass", "id": f"extra-{index}",
                 "name": "private-pagination-canary"}
                for index in range(1000)
            ]],
            "result_info": {"page": 1, "total_pages": 1},
        }], "NAMESPACE_PAGE_SIZE")

    def test_exact_counted_terminal_remainder_succeeds(self):
        pages = self.pages()
        pages[1]["result"].extend([
            {"script": "unrelated", "class": "OtherClass", "id": f"remainder-{index}"}
            for index in range(4)
        ])
        for page in pages:
            page["result_info"].update(
                count=len(page["result"]), per_page=1000, total_count=1005
            )
        self.assert_accepted(pages)

    def test_explicit_only_optional_metadata_and_absent_unrelated_id_stay_supported(self):
        for extra in ({}, {"per_page": 1000, "count": 2}):
            with self.subTest(extra=extra):
                self.assert_accepted([{
                    "result": [*fixtures()[1], {"script": "unrelated", "class": "OtherClass"}],
                    "result_info": {"page": 1, "total_pages": 1, **extra},
                }])

    def test_full_explicit_only_terminal_page_succeeds(self):
        page = self.pages()[0]
        page["result_info"]["total_pages"] = 1
        self.assert_accepted([page])

    def test_exact_target_identity_is_still_required_at_the_cli(self):
        for field, value in (("script", "nntune-alerts"), ("class", "OtherClass"),
                             ("id", "other-namespace")):
            with self.subTest(field=field):
                target = copy.deepcopy(fixtures()[1])
                target[0][field] = value
                self.assert_rejected([{"result": target,
                                       "result_info": {"page": 1, "total_pages": 1}}])


class WorkflowAcceptanceTests(unittest.TestCase):
    COMMANDS = (
        "python3 .github/scripts/test_alerts_staging.py",
        "python3 .github/scripts/test_alerts_namespace_contract.py",
    )

    def workflows(self):
        return {
            name: (guard.ROOT / ".github/workflows" / name).read_text()
            for name in ("ci.yml", "deploy-alerts-staging.yml")
        }

    def required_step(self, steps, command):
        indexes = [
            index for index, step in enumerate(steps)
            if step_level_field(step, "run") == command
        ]
        self.assertEqual(len(indexes), 1, f"Expected one executable step: {command}")
        step = steps[indexes[0]]
        self.assertIsNone(step_level_field(step, "if"), f"Acceptance cannot be skipped: {command}")
        self.assertIsNone(step_level_field(step, "continue-on-error"),
                          f"Acceptance failure must stop the job: {command}")
        return indexes[0]

    def assert_acceptance_wiring(self, workflows):
        ci = split_jobs(workflows["ci.yml"])
        self.assertIn("tail-projection", ci)
        self.assertIn("ci-ok", ci)
        self.assertIn("tail-projection", job_needs(ci["ci-ok"]))
        for command in self.COMMANDS:
            self.assertEqual(check_required_step(
                "tail-projection", ci["tail-projection"], command,
                workflow_text=workflows["ci.yml"],
            ), [])

        deploy_jobs = split_jobs(workflows["deploy-alerts-staging.yml"])
        self.assertIn("deploy", deploy_jobs)
        deploy = deploy_jobs["deploy"]
        self.assertEqual(non_plain_key_lines(deploy), [])
        self.assertIsNone(job_level_field(deploy, "continue-on-error"))
        steps = split_steps(deploy)
        config = self.required_step(steps, "python3 .github/scripts/alerts_staging.py config")
        deployments = [
            index for index, step in enumerate(steps)
            if (step_level_field(step, "uses") or "").startswith("cloudflare/wrangler-action@")
            and (step_field(step, "command") or "").split()[:1] == ["deploy"]
        ]
        self.assertEqual(len(deployments), 1, "Expected one staging deployment action")
        for command in self.COMMANDS:
            acceptance = self.required_step(steps, command)
            self.assertEqual(check_step_shell(
                "deploy", deploy, steps[acceptance], workflows["deploy-alerts-staging.yml"],
            ), [])
            self.assertLess(acceptance, config, f"Acceptance must precede config generation: {command}")
            self.assertLess(acceptance, deployments[0], f"Acceptance must precede deployment: {command}")

    def test_acceptance_is_required_in_ci_and_runs_before_config_and_deploy(self):
        self.assert_acceptance_wiring(self.workflows())

    def test_removed_or_noop_acceptance_steps_are_rejected_in_both_jobs(self):
        workflows = self.workflows()
        self.assert_acceptance_wiring(workflows)
        for filename, job_id in (("ci.yml", "tail-projection"),
                                 ("deploy-alerts-staging.yml", "deploy")):
            steps = split_steps(split_jobs(workflows[filename])[job_id])
            for command in self.COMMANDS:
                index = self.required_step(steps, command)
                step = steps[index]
                for replacement in ("", step.replace(f"run: {command}", "run: ':'")):
                    with self.subTest(workflow=filename, command=command, removed=not replacement):
                        mutated = dict(workflows)
                        mutated[filename] = workflows[filename].replace(step, replacement, 1)
                        with self.assertRaises(AssertionError):
                            self.assert_acceptance_wiring(mutated)

    def with_shell(self, workflows, filename, command, scope, shell):
        mutated = dict(workflows)
        text = workflows[filename]
        job_id = "tail-projection" if filename == "ci.yml" else "deploy"
        job = split_jobs(text)[job_id]
        steps = split_steps(job)
        # Isolate the selected bank even when it inherits an unsafe default.
        for other in self.COMMANDS:
            if other == command:
                continue
            step = steps[self.required_step(steps, other)]
            text = text.replace(step, step + "\n        shell: bash", 1)
        if scope == "step":
            step = steps[self.required_step(steps, command)]
            text = text.replace(step, step + f"\n        shell: {shell}", 1)
        elif scope == "job":
            header = f"  {job_id}:\n"
            self.assertEqual(text.count(header), 1)
            text = text.replace(header, header + f"    defaults:\n      run:\n        shell: {shell}\n", 1)
        else:
            self.assertEqual(scope, "workflow")
            text = f"defaults:\n  run:\n    shell: {shell}\n" + text
        mutated[filename] = text
        return mutated

    def test_shell_masks_are_rejected_for_each_bank_and_inheritance_scope(self):
        workflows = self.workflows()
        for filename in workflows:
            for command in self.COMMANDS:
                for scope in ("step", "job", "workflow"):
                    for shell in ("true {0}", "echo {0}", "bash -c true {0}",
                                  "sh -c 'exit 0' {0}", "python -c pass {0}",
                                  "'true {0}'", '"true {0}"'):
                        with self.subTest(filename=filename, command=command, scope=scope, shell=shell):
                            mutated = self.with_shell(workflows, filename, command, scope, shell)
                            with self.assertRaises(AssertionError):
                                self.assert_acceptance_wiring(mutated)

    def test_supported_shells_and_more_specific_overrides_stay_executable(self):
        workflows = self.workflows()
        for filename in workflows:
            job_id = "tail-projection" if filename == "ci.yml" else "deploy"
            for command in self.COMMANDS:
                for scope in ("step", "job", "workflow"):
                    for shell in ("bash", "sh", "bash -e {0}", "sh -e {0}",
                                  "bash --noprofile --norc -e -o pipefail {0}",
                                  "'bash'", '"bash"'):
                        with self.subTest(filename=filename, command=command, scope=scope, shell=shell):
                            self.assert_acceptance_wiring(
                                self.with_shell(workflows, filename, command, scope, shell)
                            )
                for scope in ("job", "workflow"):
                    with self.subTest(filename=filename, command=command, overridden=scope):
                        overridden = self.with_shell(workflows, filename, command, scope, "true {0}")
                        text = overridden[filename]
                        steps = split_steps(split_jobs(text)[job_id])
                        step = steps[self.required_step(steps, command)]
                        overridden[filename] = text.replace(step, step + "\n        shell: bash", 1)
                        self.assert_acceptance_wiring(overridden)
                with self.subTest(filename=filename, command=command, job_overrides_workflow=True):
                    overridden = self.with_shell(workflows, filename, command, "job", "bash")
                    overridden[filename] = "defaults:\n  run:\n    shell: true {0}\n" + overridden[filename]
                    self.assert_acceptance_wiring(overridden)

    def test_conditional_or_failure_masked_acceptance_is_rejected_in_both_jobs(self):
        workflows = self.workflows()
        for filename, job_id in (("ci.yml", "tail-projection"),
                                 ("deploy-alerts-staging.yml", "deploy")):
            steps = split_steps(split_jobs(workflows[filename])[job_id])
            for command in self.COMMANDS:
                step = steps[self.required_step(steps, command)]
                for extra in ("if: always()", "continue-on-error: true"):
                    with self.subTest(filename=filename, command=command, extra=extra):
                        mutated = dict(workflows)
                        mutated[filename] = workflows[filename].replace(
                            step, step + "\n        " + extra, 1,
                        )
                        with self.assertRaises(AssertionError):
                            self.assert_acceptance_wiring(mutated)

    def test_acceptance_after_config_or_deploy_is_rejected(self):
        workflows = self.workflows()
        self.assert_acceptance_wiring(workflows)
        filename = "deploy-alerts-staging.yml"
        steps = split_steps(split_jobs(workflows[filename])["deploy"])
        for command in self.COMMANDS:
            acceptance = steps[self.required_step(steps, command)]
            for boundary in ("config", "deploy"):
                if boundary == "config":
                    anchor = steps[self.required_step(
                        steps, "python3 .github/scripts/alerts_staging.py config"
                    )]
                else:
                    matches = [step for step in steps
                               if (step_level_field(step, "uses") or "").startswith(
                                   "cloudflare/wrangler-action@")]
                    self.assertEqual(len(matches), 1)
                    anchor = matches[0]
                with self.subTest(command=command, after=boundary):
                    mutated = dict(workflows)
                    text = workflows[filename].replace(acceptance + "\n", "", 1)
                    mutated[filename] = text.replace(anchor, anchor + "\n" + acceptance, 1)
                    with self.assertRaises(AssertionError):
                        self.assert_acceptance_wiring(mutated)


if __name__ == "__main__":
    unittest.main()
