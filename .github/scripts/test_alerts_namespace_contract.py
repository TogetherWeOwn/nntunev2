#!/usr/bin/env python3
"""Independent, frozen acceptance bank for namespace enumeration and identity.

Run: python3 .github/scripts/test_alerts_namespace_contract.py
Provenance: fixtures/alerts-namespace-contract.json. No live response is claimed.
The Builder consumes these files unchanged and routes test defects to their author.

The positive counterexample has complete numeric count evidence but no optional
`total_pages`. Adversarial cases must never become a successful partial inventory.
The real paginator, identity verifier and CLI run; only their network boundary is
replaced. Page size 1000 and the 100-page ceiling preserve the existing bounds.
"""

import copy
from contextlib import redirect_stdout
import io
import json
from pathlib import Path
import unittest
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit

import alerts_staging as guard
from test_ci_pnpm_retry import (
    check_required_step, job_needs, split_jobs, split_steps, step_level_field,
    with_defaults_shell, with_step_field,
)


FIXTURE = Path(__file__).with_name("fixtures") / "alerts-namespace-contract.json"
ACCOUNT = "00000000000000000000000000000000"
LOGS = (
    "https://dash.cloudflare.com/00000000000000000000000000000000/"
    "workers/services/view/nntune-api-staging/observability/logs"
)


class NamespaceContractTests(unittest.TestCase):
    def setUp(self):
        self.data = json.loads(FIXTURE.read_text())
        self.network = patch("socket.socket", side_effect=AssertionError("Offline tests forbid network"))
        self.network.start()
        self.addCleanup(self.network.stop)

    def target(self):
        return copy.deepcopy(self.data["single_page_without_total_pages"]["result"][0])

    def single_page(self, explicit_pages=False):
        page = copy.deepcopy(self.data["single_page_without_total_pages"])
        if explicit_pages:
            page["result_info"]["total_pages"] = 1
        return page

    def multi_page(self, explicit_pages=False, target_first=False):
        # Expand a documented namespace shape, not an invented short first page:
        # the requested 1000 items fill page one, with one item on page two.
        unrelated = []
        for index in range(1, 1002):
            item = copy.deepcopy(self.data["unrelated_namespace"])
            item["id"] = f"{index:032x}"
            unrelated.append(item)
        batches = ([self.target(), *unrelated[:999]], [unrelated[1000]]) if target_first else (
            unrelated[:1000], [self.target()]
        )
        pages = []
        for number, batch in enumerate(batches, 1):
            page = self.single_page()
            page["result"] = batch
            page["result_info"].update(count=len(batch), page=number, total_count=1001)
            if explicit_pages:
                page["result_info"]["total_pages"] = 2
            pages.append(page)
        return pages

    def page_reader(self, pages):
        requested = []

        def get(path):
            parts = urlsplit(path)
            self.assertEqual((parts.scheme, parts.netloc, parts.path, parts.fragment),
                             ("", "", "durable_objects/namespaces", ""))
            query = parse_qs(parts.query, strict_parsing=True)
            self.assertEqual(set(query), {"page", "per_page"})
            self.assertEqual(query["per_page"], ["1000"])
            self.assertEqual(query["page"], [str(len(requested) + 1)])
            number = int(query["page"][0])
            requested.append(number)
            if number <= len(pages):
                return copy.deepcopy(pages[number - 1])
            # Permit the SDK's extra empty-page read as well as count-based
            # termination. Neither success strategy is prescribed by the bank.
            self.assertEqual(number, len(pages) + 1, "Unbounded or repeated pagination")
            terminal = self.single_page()
            terminal["result"] = []
            terminal["result_info"].update(
                count=0, page=number,
                total_count=sum(len(p["result"]) for p in pages if isinstance(p.get("result"), list)),
            )
            return terminal

        return get, requested

    def collect_supported(self, pages):
        get, requested = self.page_reader(pages)
        try:
            namespaces = guard.list_namespaces(get)
        except guard.GuardError as error:
            self.fail(f"Documented, count-complete response was rejected: {error}")
        return namespaces, requested

    def verify(self, namespaces):
        return guard.verify(self.data["settings"], namespaces, self.data["subdomain"],
                            self.data["versions"], LOGS)

    def run_cli(self, pages):
        read_page, requested = self.page_reader(pages)
        routes = {
            "scripts/nntune-alerts-staging/settings": self.data["settings"],
            "scripts/nntune-alerts-staging/subdomain": self.data["subdomain"],
            "scripts/nntune-alerts-staging/versions?per_page=1": self.data["versions"],
        }

        def api_get(path, account_id, token):
            self.assertEqual(account_id, ACCOUNT)
            self.assertEqual(token, "")  # No credentials are needed or inherited.
            if urlsplit(path).path == "durable_objects/namespaces":
                return read_page(path)
            self.assertIn(path, routes, "Unexpected endpoint in the offline verifier")
            return {"success": True, "result": copy.deepcopy(routes[path])}

        output = io.StringIO()
        with patch.object(guard, "api_get", side_effect=api_get), \
             patch.object(guard.os, "environ", {"CLOUDFLARE_ACCOUNT_ID": ACCOUNT}), \
             patch.object(guard.sys, "argv", ["alerts_staging.py", "verify"]), \
             redirect_stdout(output):
            code = guard.main()
        report = json.loads(output.getvalue())
        return code, report, requested

    def assert_rejected(self, pages):
        code, report, requested = self.run_cli(pages)
        self.assertEqual(code, 1, f"Unsafe enumeration was accepted: {report}")
        self.assertIs(report["ok"], False)
        self.assertIs(report["retryable"], False)
        self.assertIsInstance(report["code"], str)
        self.assertNotIn("private-pagination-canary", json.dumps(report))
        self.assertNotIn("version_id", report)
        self.assertLessEqual(len(requested), 100)

    def test_count_complete_single_page_does_not_require_optional_total_pages(self):
        namespaces, requested = self.collect_supported([self.single_page()])
        self.assertEqual(namespaces, [self.target()])
        self.assertIn(requested, ([1], [1, 2]))
        self.assertEqual(self.verify(namespaces)["do_namespace_id"], "ffffffffffffffffffffffffffffffff")

    def test_count_complete_multi_page_includes_the_target_on_page_two(self):
        namespaces, requested = self.collect_supported(self.multi_page())
        self.assertIn(requested, ([1, 2], [1, 2, 3]))
        self.assertEqual(len(namespaces), 1001)
        self.assertEqual(len({namespace["id"] for namespace in namespaces}), 1001)
        self.assertEqual(namespaces[-1], self.target())
        self.assertEqual(self.verify(namespaces)["do_namespace_id"], "ffffffffffffffffffffffffffffffff")

    def test_full_count_complete_page_has_no_phantom_missing_page(self):
        page = self.multi_page(target_first=True)[0]
        page["result_info"]["total_count"] = 1000
        namespaces, requested = self.collect_supported([page])
        self.assertEqual(len(namespaces), 1000)
        self.assertIn(requested, ([1], [1, 2]))
        self.assertEqual(self.verify(namespaces)["do_namespace_id"], "ffffffffffffffffffffffffffffffff")

    def test_existing_explicit_total_pages_contract_remains_supported(self):
        page = self.single_page()
        page["result_info"] = {"page": 1, "total_pages": 1}
        namespaces, requested = self.collect_supported([page])
        self.assertEqual(namespaces, [self.target()])
        self.assertIn(requested, ([1], [1, 2]))

    def test_cli_success_requires_complete_counted_enumeration(self):
        code, report, requested = self.run_cli(self.multi_page())
        self.assertEqual(code, 0, f"Supported pagination failed at the real CLI: {report}")
        self.assertEqual(report, {
            "ok": True, "worker": "nntune-alerts-staging",
            "version_id": "00000000-0000-4000-8000-000000000001",
            "do_namespace_id": "ffffffffffffffffffffffffffffffff",
            "bindings": ["ALERT_WINDOWS", "LOGS_URL", "PRODUCER_SCRIPT_NAME"],
        })
        self.assertIn(requested, ([1, 2], [1, 2, 3]))

    def test_missing_or_incomplete_metadata_is_not_single_page_success(self):
        for info in (None, {}, [], "private-pagination-canary", {"page": 1},
                     {"page": 1, "count": 1, "per_page": 1000}):
            with self.subTest(info=info):
                page = self.single_page()
                page["result_info"] = info
                self.assert_rejected([page])
        page = self.single_page()
        del page["result_info"]
        self.assert_rejected([page])

    def test_present_numeric_metadata_is_not_coerced_or_ignored(self):
        cases = (
            ("page", True), ("page", 1.0), ("page", "1"), ("page", 0),
            ("per_page", True), ("per_page", 1000.0), ("per_page", "1000"),
            ("per_page", 0), ("per_page", -1), ("per_page", 1001),
            ("count", True), ("count", 1.0), ("count", "1"), ("count", -1),
            ("total_count", True), ("total_count", 1.0), ("total_count", "1"),
            ("total_count", -1),
            ("total_pages", True), ("total_pages", 1.0), ("total_pages", "1"),
            ("total_pages", 0), ("total_pages", 101),
        )
        for key, value in cases:
            with self.subTest(key=key, value=value):
                page = self.single_page(explicit_pages=True)
                page["result_info"][key] = value
                self.assert_rejected([page])

    def test_explicit_pages_do_not_hide_contradictory_count_evidence(self):
        for changes in ({"count": 0}, {"count": 2}, {"total_count": 0},
                        {"total_count": 1001}, {"total_pages": 2}, {"per_page": 999}):
            with self.subTest(changes=changes):
                page = self.single_page(explicit_pages=True)
                page["result_info"].update(changes)
                self.assert_rejected([page])

    def test_metadata_must_remain_consistent_between_pages(self):
        for key, value in (("page", 1), ("page", True), ("total_count", 1000),
                           ("total_count", 1002), ("total_pages", 1),
                           ("total_pages", 3), ("per_page", 500)):
            with self.subTest(key=key, value=value):
                pages = self.multi_page(explicit_pages=True, target_first=True)
                pages[1]["result_info"][key] = value
                self.assert_rejected(pages)

    def test_empty_page_before_advertised_completion_is_not_success(self):
        pages = self.multi_page(explicit_pages=True, target_first=True)
        pages[1]["result"] = []
        pages[1]["result_info"]["count"] = 0
        self.assert_rejected(pages)

    def test_short_nonterminal_page_is_not_a_complete_inventory(self):
        pages = self.multi_page(explicit_pages=True, target_first=True)
        pages[0]["result"] = [self.target()]
        pages[0]["result_info"]["count"] = 1
        self.assert_rejected(pages)

    def test_malformed_later_page_is_checked_even_after_target_was_found(self):
        for batch in (None, {}, "private-pagination-canary"):
            with self.subTest(batch=batch):
                pages = self.multi_page(explicit_pages=True, target_first=True)
                pages[1]["result"] = batch
                self.assert_rejected(pages)

    def test_overlapping_namespace_ids_are_not_counted_as_distinct_results(self):
        pages = self.multi_page(explicit_pages=True, target_first=True)
        pages[1]["result"] = [copy.deepcopy(pages[0]["result"][1])]
        self.assert_rejected(pages)

    def test_target_on_page_one_does_not_hide_a_second_matching_namespace(self):
        for explicit_pages in (False, True):
            with self.subTest(explicit_pages=explicit_pages):
                pages = self.multi_page(explicit_pages=explicit_pages, target_first=True)
                pages[1]["result"] = [self.target()]
                pages[1]["result"][0]["id"] = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
                get, _ = self.page_reader(pages)
                with self.assertRaisesRegex(guard.GuardError, "DO_NAMESPACE_COUNT"):
                    self.verify(guard.list_namespaces(get))

    def test_worker_class_and_binding_namespace_identity_remain_exact(self):
        for key, value, code in (
            ("script", "fixture-other-worker", "DO_NAMESPACE_COUNT"),
            ("class", "FixtureWrongClass", "DO_NAMESPACE_CLASS"),
            ("id", "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", "DO_NAMESPACE_ID"),
        ):
            with self.subTest(key=key):
                pages = self.multi_page(explicit_pages=True)
                pages[1]["result"][0][key] = value
                get, _ = self.page_reader(pages)
                with self.assertRaisesRegex(guard.GuardError, code):
                    self.verify(guard.list_namespaces(get))

    def test_counted_pagination_cannot_exceed_the_existing_page_budget(self):
        requested = []

        def get(path):
            query = parse_qs(urlsplit(path).query, strict_parsing=True)
            number = int(query["page"][0])
            requested.append(number)
            self.assertLessEqual(len(requested), 100, "Pagination exceeded its existing request budget")
            page = self.single_page()
            page["result"] = []
            for index in range(1000):
                item = copy.deepcopy(self.data["unrelated_namespace"])
                item["id"] = f"{(number - 1) * 1000 + index + 1:032x}"
                page["result"].append(item)
            page["result_info"].update(count=1000, page=number, total_count=100001)
            return page

        with self.assertRaises(guard.GuardError):
            guard.list_namespaces(get)
        self.assertLessEqual(len(requested), 100)

    def test_counted_pagination_allows_the_exact_page_budget(self):
        # The existing 100-page ceiling still permits a full 100-page
        # counted inventory: 100000 items in exactly 100 requests.
        requested = []

        def get(path):
            query = parse_qs(urlsplit(path).query, strict_parsing=True)
            number = int(query["page"][0])
            requested.append(number)
            self.assertLessEqual(len(requested), 100, "Pagination exceeded its existing request budget")
            page = self.single_page()
            page["result"] = []
            for index in range(1000):
                if number == 100 and index == 999:
                    page["result"].append(self.target())
                else:
                    item = copy.deepcopy(self.data["unrelated_namespace"])
                    item["id"] = f"{(number - 1) * 1000 + index + 1:032x}"
                    page["result"].append(item)
            page["result_info"].update(count=1000, page=number, total_count=100000)
            return page

        try:
            namespaces = guard.list_namespaces(get)
        except guard.GuardError as error:
            self.fail(f"Exact 100-page counted inventory was rejected: {error}")
        self.assertEqual(requested, list(range(1, 101)))
        self.assertEqual(len(namespaces), 100000)
        self.assertEqual(len({namespace["id"] for namespace in namespaces}), 100000)
        self.assertEqual(namespaces[-1], self.target())
        self.assertEqual(self.verify(namespaces)["do_namespace_id"], "ffffffffffffffffffffffffffffffff")


class AcceptanceCheckerWiringTests(unittest.TestCase):
    """The tail-projection bank survives deletion/masking of the public checker."""

    COMMAND = "python3 .github/scripts/test_ci_pnpm_retry.py"

    def workflow(self):
        return (Path(__file__).parents[1] / "workflows/ci.yml").read_text()

    def assert_public_checker(self, text):
        jobs = split_jobs(text)
        for job in ("public-text", "tail-projection", "ci-ok"):
            self.assertIn(job, jobs)
        for job in ("public-text", "tail-projection"):
            self.assertIn(job, job_needs(jobs["ci-ok"]))
        self.assertEqual(check_required_step(
            "public-text", jobs["public-text"], self.COMMAND, workflow_text=text,
        ), [])

    def test_public_checker_runs_as_a_required_independently_backstopped_step(self):
        self.assert_public_checker(self.workflow())

    def test_composed_bank_removal_and_public_checker_masks_are_rejected(self):
        text = """jobs:
  public-text:
    runs-on: ubuntu-latest
    steps:
      - run: python3 .github/scripts/test_ci_pnpm_retry.py
  tail-projection:
    runs-on: ubuntu-latest
    steps:
      - run: python3 .github/scripts/test_alerts_staging.py
      - run: python3 .github/scripts/test_alerts_namespace_contract.py
  ci-ok:
    needs: [public-text, tail-projection]
"""
        self.assert_public_checker(text)
        jobs = split_jobs(text)
        removed = next(step for step in split_steps(jobs["tail-projection"])
                       if step_level_field(step, "run") == "python3 .github/scripts/test_alerts_staging.py")
        survivor = next(step for step in split_steps(jobs["tail-projection"])
                        if step_level_field(step, "run") == "python3 .github/scripts/test_alerts_namespace_contract.py")
        # The namespace bank remains executable even under a masked workflow default.
        text = text.replace(removed, "", 1).replace(survivor, with_step_field(survivor, "shell", "bash"), 1)
        public = split_jobs(text)["public-text"]
        checker = next(step for step in split_steps(public)
                       if step_level_field(step, "run") == self.COMMAND)
        for replacement in ("", checker.replace(self.COMMAND, ":"),
                            with_step_field(checker, "if", "always()"),
                            with_step_field(checker, "continue-on-error", "true"),
                            with_step_field(checker, "shell", "true {0}")):
            with self.subTest(replacement=replacement), self.assertRaises(AssertionError):
                self.assert_public_checker(text.replace(checker, replacement, 1))
        for scope in ("job", "workflow"):
            masked = text.replace(checker, with_step_field(checker, "shell", None), 1)
            job = split_jobs(masked)["public-text"]
            masked = masked.replace(job, with_defaults_shell(job, "true {0}" if scope == "job" else None, 4), 1)
            if scope == "workflow":
                masked = with_defaults_shell(masked, "true {0}", 0)
            with self.subTest(inherited=scope), self.assertRaises(AssertionError):
                self.assert_public_checker(masked)
            # More-specific supported overrides must still execute the checker.
            job = split_jobs(masked)["public-text"]
            step = next(step for step in split_steps(job)
                        if step_level_field(step, "run") == self.COMMAND)
            self.assert_public_checker(masked.replace(step, with_step_field(step, "shell", "bash"), 1))


if __name__ == "__main__":
    unittest.main()
