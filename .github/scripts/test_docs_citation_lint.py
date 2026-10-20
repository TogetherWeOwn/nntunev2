"""Unit tests for docs_citation_lint.

Fixture repos under fixtures/docs-citations/ are shared with the CI job:
``match`` must pass; ``broken-path``, ``broken-eof``, ``broken-order`` and
``broken-symbol`` must fail, each with its own reason. The remaining cases
build throwaway mini-repos in a temp directory.
"""

import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from docs_citation_lint import (
    LintError,
    check_source,
    citation_occurrences,
    extract_anchor,
    is_citation_content,
    main,
    markdown_files,
    parse_ranges,
    split_citation,
)

SCRIPTS = Path(__file__).resolve().parent
FIXTURES = SCRIPTS / "fixtures" / "docs-citations"
REPO_ROOT = SCRIPTS.parents[1]


def make_root(tmp, files):
    root = Path(tmp)
    for name, text in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return root


def check(root, source):
    return check_source(Path(source), root, {})


def occurrences(root, source):
    return citation_occurrences(Path(source), root)


LIB_RS = """//! Demo crate.

/// Maximum retries.
pub const MAX_RETRIES: u32 = 3;

/// Run the job.
pub fn run_job() -> bool {
    helper_inner()
}

fn helper_inner() -> bool {
    true
}
"""


class TestCitationShape(unittest.TestCase):
    def test_plain_and_ranged_paths_are_citations(self):
        for content in (
            "src/lib.rs:4",
            "src/lib.rs:7-9",
            "workers/api/src/vault.rs:361-447",
            "docs/ops.md:112-113",
            "lib.rs:21",
            "config_parity_check.py:144-148",
            "Cargo.toml:2",
            "./Dockerfile:12",
        ):
            self.assertTrue(is_citation_content(content), content)

    def test_comma_unions_are_citations(self):
        self.assertTrue(is_citation_content("wrangler.toml:34-36,59-61"))
        self.assertTrue(is_citation_content("lib.rs:126,162,182"))

    def test_non_citations_are_skipped(self):
        for content in (
            ":335",
            ":87-108",
            "localhost:3000",
            "nntp:119",
            "12:00:00",
            "18:46:25Z",
            "2026-01-13T00:00:00Z",
            "127.0.0.1:8080",
            "3 May at 14:20",
            "file.md#section",
            "just prose",
        ):
            self.assertFalse(is_citation_content(content), content)

    def test_ranges_parse(self):
        self.assertEqual([(4, 4)], parse_ranges("4"))
        self.assertEqual([(7, 9)], parse_ranges("7-9"))
        self.assertEqual([(34, 36), (59, 61)], parse_ranges("34-36,59-61"))
        path, parts = split_citation("src/lib.rs:7-9")
        self.assertEqual("src/lib.rs", path)
        self.assertEqual([(7, 9)], parts)


class TestAnchorExtraction(unittest.TestCase):
    def test_identifier_shapes(self):
        self.assertEqual("validate", extract_anchor("IndexerConfig::validate"))
        self.assertEqual("connect", extract_anchor("NntpClient::connect"))
        self.assertEqual(
            "INDEXER_ALLOW_REDIRECTS",
            extract_anchor("INDEXER_ALLOW_REDIRECTS = false"),
        )
        self.assertEqual("MAX_REDIRECTS", extract_anchor("MAX_REDIRECTS = 3"))
        self.assertEqual(
            "indexer_fetch", extract_anchor("pub mod indexer_fetch")
        )
        self.assertEqual("authorize_pool_dial", extract_anchor("authorize_pool_dial"))
        self.assertEqual("ip_is_forbidden", extract_anchor("ip_is_forbidden"))
        self.assertEqual("https", extract_anchor("https"))
        self.assertEqual("TYPES", extract_anchor("TYPES"))
        self.assertEqual("ConfigType", extract_anchor("ConfigType"))
        self.assertEqual("is_redirect", extract_anchor("is_redirect()"))

    def test_non_anchors(self):
        for content in (
            "workers/api/src/vault.rs",
            "https://example.com/x",
            "crates/nntune-config",
            "test connection",
            "exp - now",
            "NOT YET DEPLOYED",
            "ManifestEntry.segments",
            "POST /repair",
            "@",
            ":443",
            "file.md#section",
        ):
            self.assertIsNone(extract_anchor(content), content)


class TestLocalAnchors(unittest.TestCase):
    def _anchors(self, doc_line):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {"docs/a.md": doc_line + "\n", "src/lib.rs": LIB_RS},
            )
            occs = occurrences(root, "docs/a.md")
            self.assertEqual(1, len(occs))
            return occs[0]["anchors"]

    def test_ident_before_paren_citation(self):
        self.assertEqual(
            {"run_job"},
            self._anchors("Run `run_job` (`src/lib.rs:7-9`) first."),
        )

    def test_citation_before_paren_ident(self):
        self.assertEqual(
            {"MAX_RETRIES"},
            self._anchors("Cap `src/lib.rs:4` (`MAX_RETRIES = 3`) applies."),
        )

    def test_paren_pair_with_comma(self):
        self.assertEqual(
            {"helper_inner"},
            self._anchors("Check (`helper_inner`, `src/lib.rs:11-13`) now."),
        )

    def test_paren_pair_with_glue_word(self):
        self.assertEqual(
            {"run_job"},
            self._anchors("Check (`run_job` in `src/lib.rs:7-9`) now."),
        )

    def test_bare_glue_word_without_parens(self):
        self.assertEqual(
            {"helper_inner"},
            self._anchors("See `helper_inner` in `src/lib.rs:11-13`."),
        )

    def test_and_enumeration_anchors_every_link(self):
        self.assertEqual(
            {"run_job", "helper_inner"},
            self._anchors("Both `run_job` and `helper_inner` (`src/lib.rs:7-13`)."),
        )

    def test_slash_enumeration_anchors_every_link(self):
        self.assertEqual(
            {"run_job", "helper_inner"},
            self._anchors("Either `run_job`/`helper_inner` (`src/lib.rs:7-13`)."),
        )

    def test_comma_enumeration_anchors_every_link(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "Pinned by `run_job`, `helper_inner` in "
                        "`src/lib.rs:7-13`.\n"
                    ),
                    "src/lib.rs": LIB_RS,
                },
            )
            occs = occurrences(root, "docs/a.md")
            self.assertEqual({"run_job", "helper_inner"}, occs[0]["anchors"])

    def test_sources_list_does_not_anchor(self):
        self.assertEqual(
            set(),
            self._anchors(
                "(Sources: `src/lib.rs:7-9` for run, `helper_inner` later.)"
            ),
        )

    def test_loose_mention_does_not_anchor(self):
        self.assertEqual(
            set(),
            self._anchors(
                "The `helper_inner` check lives far away (`src/lib.rs:7-9`)."
            ),
        )

    def test_citation_neighbor_is_not_an_anchor(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "First `src/lib.rs:4`, then `src/lib.rs:7-9`.\n",
                    "src/lib.rs": LIB_RS,
                },
            )
            occs = occurrences(root, "docs/a.md")
            self.assertEqual([set(), set()], [o["anchors"] for o in occs])

    def test_pair_with_extra_spans_does_not_anchor(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "(`helper_inner`, `src/lib.rs:4`; also "
                        "`src/lib.rs:7-9`).\n"
                    ),
                    "src/lib.rs": LIB_RS,
                },
            )
            occs = occurrences(root, "docs/a.md")
            self.assertEqual(2, len(occs))
            self.assertEqual(set(), occs[1]["anchors"])

    def test_anchor_spans_soft_wrap(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "Cap `MAX_RETRIES = false`\n(`src/lib.rs:4`) here.\n"
                    ),
                    "src/lib.rs": LIB_RS,
                },
            )
            occs = occurrences(root, "docs/a.md")
            self.assertEqual({"MAX_RETRIES"}, occs[0]["anchors"])
            self.assertEqual(2, occs[0]["lineno"])


class TestLanding(unittest.TestCase):
    def test_symbol_at_start_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "Run `run_job` (`src/lib.rs:7-9`).\n",
                    "src/lib.rs": LIB_RS,
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_one_line_doc_lead_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "Cap (`MAX_RETRIES = 3`, `src/lib.rs:3-4`).\n",
                    "src/lib.rs": LIB_RS,
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_two_line_drift_flags(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "Run `run_job` (`src/lib.rs:5-9`).\n",
                    "src/lib.rs": LIB_RS,
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))
            self.assertIn("does not land on run_job", hits[0][2])

    def test_single_line_tolerates_doc_lead(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "Cap (`MAX_RETRIES = 3`, `src/lib.rs:3`).\n",
                    "src/lib.rs": LIB_RS,
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_single_line_drift_flags(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "Run `run_job` (`src/lib.rs:5`).\n",
                    "src/lib.rs": LIB_RS,
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))

    def test_any_chained_anchor_landing_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "Both `run_job` and `helper_inner` "
                        "(`src/lib.rs:7-13`).\n"
                    ),
                    "src/lib.rs": LIB_RS,
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_each_union_part_must_land(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "Cap `src/lib.rs:4,7` (`MAX_RETRIES`).\n",
                    "src/lib.rs": LIB_RS,
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))
            self.assertIn("MAX_RETRIES", hits[0][2])

    def test_matching_is_case_insensitive(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {"docs/a.md": "Cap (`max_retries`, `src/lib.rs:4`).\n",
                 "src/lib.rs": LIB_RS},
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_matching_is_word_boundary(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "Run `run` (`src/lib.rs:7-9`).\n",
                    "src/lib.rs": LIB_RS,
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))

    def test_unanchored_citation_checks_range_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {"docs/a.md": "See `src/lib.rs:7-9` for details.\n",
                 "src/lib.rs": LIB_RS},
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_past_eof_flags(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {"docs/a.md": "See `src/lib.rs:99`.\n", "src/lib.rs": LIB_RS},
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))
            self.assertIn("out of range", hits[0][2])

    def test_reversed_and_zero_ranges_flag(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "See `src/lib.rs:9-7` and `src/lib.rs:0`.\n",
                    "src/lib.rs": LIB_RS,
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(2, len(hits))
            for hit in hits:
                self.assertIn("invalid citation range", hit[2])


class TestResolution(unittest.TestCase):
    def test_docdir_beats_root(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "See `Cargo.toml:2`.\n",
                    "docs/Cargo.toml": "[package]\nname = \"docs\"\n",
                    "Cargo.toml": "[workspace]\n",
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_crate_src_fallback(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "workers/demo/README.md": "See `lib.rs:7-9`.\n",
                    "workers/demo/Cargo.toml": "[package]\n",
                    "workers/demo/src/lib.rs": LIB_RS,
                },
            )
            self.assertEqual([], check(root, "workers/demo/README.md"))

    def test_crate_relative_src_path(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "workers/demo/README.md": "See `src/lib.rs:7-9`.\n",
                    "workers/demo/Cargo.toml": "[package]\n",
                    "workers/demo/src/lib.rs": LIB_RS,
                },
            )
            self.assertEqual([], check(root, "workers/demo/README.md"))

    def test_root_relative_from_nested_doc(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "workers/demo/README.md": (
                        "See `workers/demo/src/lib.rs:7-9`.\n"
                    ),
                    "workers/demo/Cargo.toml": "[package]\n",
                    "workers/demo/src/lib.rs": LIB_RS,
                },
            )
            self.assertEqual([], check(root, "workers/demo/README.md"))

    def test_twin_basename_resolves(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "Full `scripts/check.py:1-2` first, then "
                        "`check.py:1-2`.\n"
                    ),
                    "scripts/check.py": "#!/usr/bin/env python3\nVALID = True\n",
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_ambiguous_twin_flags(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "Full `one/check.py:1` and `two/check.py:1`, then "
                        "`check.py:1`.\n"
                    ),
                    "one/check.py": "A = 1\n",
                    "two/check.py": "B = 2\n",
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))
            self.assertIn("does not resolve", hits[0][2])
            self.assertIn("check.py:1", hits[0][1])

    def test_unresolvable_path_flags(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(tmp, {"docs/a.md": "See `nope/missing.rs:1`.\n"})
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))
            self.assertIn("does not resolve", hits[0][2])

    def test_dotdot_escape_does_not_leave_root(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp, {"docs/a.md": "See `../../../../etc/hostname:1`.\n"}
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))
            self.assertIn("does not resolve", hits[0][2])


class TestHostResolution(unittest.TestCase):
    def test_unresolved_recognized_hosts_are_skipped(self):
        for citation in ("db.example.com:5432", "ghcr.io/org/image:2"):
            with self.subTest(citation=citation), tempfile.TemporaryDirectory() as tmp:
                root = make_root(tmp, {"docs/a.md": f"Connect to `{citation}`.\n"})
                self.assertEqual([], check(root, "docs/a.md"))

    def test_real_hostlike_file_checks_eof_before_skip(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "See `example.com:99`.\n",
                    "docs/example.com": "one\ntwo\n",
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual([(1, "example.com:99")], [h[:2] for h in hits])
            self.assertIn("out of range", hits[0][2])

    def test_real_hostlike_file_checks_symbol_before_skip(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "Run `run_job` (`example.com:1`).\n",
                    "docs/example.com": "other\nother\nrun_job\n",
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual([(1, "example.com:1")], [h[:2] for h in hits])
            self.assertIn("does not land on run_job", hits[0][2])

    def test_hostlike_basename_twin_checks_eof_before_skip(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "Source `scripts/example.com`.\n\n"
                        "See `example.com:99`.\n"
                    ),
                    "scripts/example.com": "one\ntwo\n",
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual([(3, "example.com:99")], [h[:2] for h in hits])
            self.assertIn("out of range", hits[0][2])

    def test_hostlike_basename_twin_checks_symbol_before_skip(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "Source `scripts/example.com`.\n\n"
                        "Run `run_job` (`example.com:1`).\n"
                    ),
                    "scripts/example.com": "other\nother\nrun_job\n",
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual([(3, "example.com:1")], [h[:2] for h in hits])
            self.assertIn("does not land on run_job", hits[0][2])

    def test_near_miss_host_suffixes_remain_unresolved(self):
        for citation in ("example.comm:99", "net.example.rs:99"):
            with self.subTest(citation=citation), tempfile.TemporaryDirectory() as tmp:
                root = make_root(tmp, {"docs/a.md": f"See `{citation}`.\n"})
                self.assertEqual(
                    [(1, citation, "citation target does not resolve")],
                    check(root, "docs/a.md"),
                )

    def test_real_hostlike_file_landing_passes(self):
        for line in (2, 3):
            with self.subTest(line=line), tempfile.TemporaryDirectory() as tmp:
                root = make_root(
                    tmp,
                    {
                        "docs/a.md": f"Run `run_job` (`example.com:{line}`).\n",
                        "docs/example.com": "other\nother\nrun_job\n",
                    },
                )
                self.assertEqual([], check(root, "docs/a.md"))

    def test_hostlike_basename_twin_landing_passes(self):
        for line in (2, 3):
            with self.subTest(line=line), tempfile.TemporaryDirectory() as tmp:
                root = make_root(
                    tmp,
                    {
                        "docs/a.md": (
                            "Source `scripts/example.com`.\n\n"
                            f"Run `run_job` (`example.com:{line}`).\n"
                        ),
                        "scripts/example.com": "other\nother\nrun_job\n",
                    },
                )
                self.assertEqual([], check(root, "docs/a.md"))


class TestTwinAnchors(unittest.TestCase):
    def test_bare_twin_inherits_anchor(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "The `Widget::build` (`src/widget.rs:19-21`) builds.\n"
                        "\n"
                        "Assembly (`src/widget.rs:10-12`) details.\n"
                    ),
                    "src/widget.rs": (
                        "\n".join(f"// filler {n}" for n in range(18))
                        + "\npub fn build() -> usize {\n    40\n}\n"
                    ),
                },
            )
            # Different ranges: no twin relation, the bare cite passes.
            self.assertEqual([], check(root, "docs/a.md"))

    def test_identical_bare_twin_flags(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "The `Widget::build` (`src/widget.rs:10-12`) builds.\n"
                        "\n"
                        "Assembly (`src/widget.rs:10-12`) details.\n"
                    ),
                    "src/widget.rs": (
                        "\n".join(f"// filler {n}" for n in range(18))
                        + "\npub fn build() -> usize {\n    40\n}\n"
                    ),
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(2, len(hits))
            self.assertIn("twin-anchored", hits[1][2])

    def test_twins_do_not_cross_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "The `Widget::build` (`src/widget.rs:19-21`).\n",
                    "docs/b.md": "Assembly (`src/widget.rs:19-21`).\n",
                    "src/widget.rs": (
                        "\n".join(f"// filler {n}" for n in range(18))
                        + "\npub fn build() -> usize {\n    40\n}\n"
                    ),
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))
            self.assertEqual([], check(root, "docs/b.md"))


class TestUnits(unittest.TestCase):
    def test_blank_line_prevents_adjacent_anchor_binding(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "Run `run_job`\n\n"
                        "(`src/lib.rs:4`) holds the cap.\n"
                    ),
                    "src/lib.rs": LIB_RS,
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))
            occs = occurrences(root, "docs/a.md")
            self.assertEqual([set()], [o["anchors"] for o in occs])
            self.assertEqual([3], [o["lineno"] for o in occs])

    def test_soft_wrap_preserves_adjacent_anchor_binding(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "Run `run_job`\n(`src/lib.rs:7`) first.\n",
                    "src/lib.rs": LIB_RS,
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))
            occs = occurrences(root, "docs/a.md")
            self.assertEqual([{"run_job"}], [o["anchors"] for o in occs])
            self.assertEqual([2], [o["lineno"] for o in occs])

    def test_historical_exemption_does_not_cross_blank_line(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "On the examined head, this was valid.\n\n"
                        "See `src/missing.rs:1`.\n"
                    ),
                },
            )
            self.assertEqual(
                [(3, "src/missing.rs:1",
                  "citation target does not resolve")],
                check(root, "docs/a.md"),
            )

    def test_historical_exemption_does_not_cross_table_row(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "| On the examined head | old |\n"
                        "| Current | `src/missing.rs:1` |\n"
                    ),
                },
            )
            self.assertEqual(
                [(2, "src/missing.rs:1",
                  "citation target does not resolve")],
                check(root, "docs/a.md"),
            )

    def test_historical_exemption_does_not_cross_bullet(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "- On the examined head, this was valid.\n"
                        "- See `src/missing.rs:1`.\n"
                    ),
                },
            )
            self.assertEqual(
                [(2, "src/missing.rs:1",
                  "citation target does not resolve")],
                check(root, "docs/a.md"),
            )

    def test_same_unit_historical_exemption_still_applies(self):
        for text in (
            "On the examined head, see `src/missing.rs:1`.\n",
            "| On the examined head | `src/missing.rs:1` |\n",
            "- On the examined head, see `src/missing.rs:1`.\n",
        ):
            with self.subTest(text=text), tempfile.TemporaryDirectory() as tmp:
                root = make_root(tmp, {"docs/a.md": text})
                self.assertEqual([], check(root, "docs/a.md"))

    def test_anchors_do_not_cross_table_rows(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "| Name | Where |\n"
                        "|---|---|\n"
                        "| `run_job` | somewhere |\n"
                        "| job | (`src/lib.rs:7-9`) |\n"
                    ),
                    "src/lib.rs": LIB_RS,
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_anchors_do_not_cross_bullets(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "- Uses `run_job` for work.\n"
                        "- See (`src/lib.rs:4`).\n"
                    ),
                    "src/lib.rs": LIB_RS,
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_fenced_citations_are_ignored(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "```md\n`src/missing.rs:1`\n```\n\nSee `src/lib.rs:7-9`.\n"
                    ),
                    "src/lib.rs": LIB_RS,
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_historical_unit_skips_broken_citations(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": (
                        "On the examined head, see `src/missing.rs:1-99`.\n"
                    ),
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_violation_line_numbers_are_exact(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {"docs/a.md": "# T\n\nSee `src/missing.rs:1`.\n",
                 "src/lib.rs": LIB_RS},
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(3, hits[0][0])


class TestMainExitCodes(unittest.TestCase):
    def test_clean_root_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {"docs/a.md": "See `src/lib.rs:7-9`.\n", "src/lib.rs": LIB_RS},
            )
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(0, main(["--root", str(root)]))

    def test_broken_root_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(tmp, {"docs/a.md": "See `src/missing.rs:1`.\n"})
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(1, main(["--root", str(root)]))

    def test_missing_root_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(
                    2, main(["--root", str(Path(tmp) / "nope")])
                )

    def test_annotation_format_is_exact(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(tmp, {"docs/a.md": "See `src/missing.rs:1`.\n"})
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                rc = main(["--root", str(root), "--files", "docs/a.md"])
            self.assertEqual(1, rc)
            self.assertIn(
                "::error file=docs/a.md,line=1::docs-citations: "
                "citation target does not resolve: src/missing.rs:1",
                buf.getvalue(),
            )

    def test_only_changed_files_are_checked(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            env = dict(
                os.environ,
                GIT_CONFIG_NOSYSTEM="1",
                GIT_AUTHOR_NAME="t",
                GIT_AUTHOR_EMAIL="t@t",
                GIT_COMMITTER_NAME="t",
                GIT_COMMITTER_EMAIL="t@t",
            )
            env.pop("GIT_DIR", None)
            env.pop("GIT_WORK_TREE", None)
            env.pop("GIT_INDEX_FILE", None)
            env.pop("GIT_COMMON_DIR", None)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True, env=env)
            (root / "docs").mkdir()
            (root / "docs" / "stale.md").write_text("See `gone/missing.rs:1`.\n")
            (root / "docs" / "fresh.md").write_text("See `src/lib.rs:7-9`.\n")
            (root / "src").mkdir()
            (root / "src" / "lib.rs").write_text(LIB_RS)
            subprocess.run(["git", "add", "."], cwd=root, check=True, env=env)
            subprocess.run(
                ["git", "commit", "-qm", "base"], cwd=root, check=True, env=env
            )
            (root / "docs" / "fresh.md").write_text(
                "See `src/lib.rs:7-9`.\n\nmore prose\n"
            )
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(
                    0, main(["--root", str(root), "--base-ref", "HEAD"])
                )


class TestReadFailures(unittest.TestCase):
    def test_unreadable_target_fails_named(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(tmp, {"docs/a.md": "See `src/lib.rs:1`.\n"})
            (root / "src").mkdir()
            (root / "src" / "lib.rs").write_bytes(b"ok\n\xff bad\n")
            with self.assertRaises(LintError) as ctx:
                check(root, "docs/a.md")
            self.assertIn("src/lib.rs", str(ctx.exception))
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                rc = main(["--root", str(root), "--files", "docs/a.md"])
            self.assertEqual(1, rc)
            self.assertIn("src/lib.rs", buf.getvalue())
            self.assertNotIn("Traceback", buf.getvalue())


class TestFixtureRepos(unittest.TestCase):
    def test_match_fixture_passes(self):
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(0, main(["--root", str(FIXTURES / "match")]))

    def test_broken_path_fixture_fails(self):
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(
                1, main(["--root", str(FIXTURES / "broken-path")])
            )

    def test_broken_eof_fixture_fails(self):
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(1, main(["--root", str(FIXTURES / "broken-eof")]))

    def test_broken_order_fixture_fails(self):
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(
                1, main(["--root", str(FIXTURES / "broken-order")])
            )

    def test_broken_symbol_fixture_fails(self):
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(
                1, main(["--root", str(FIXTURES / "broken-symbol")])
            )

    def test_real_repo_docs_are_clean(self):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            rc = main(["--root", str(REPO_ROOT)])
        self.assertEqual(0, rc, buf.getvalue())

    def test_lint_fixtures_are_excluded_from_full_scan(self):
        self.assertEqual(
            [],
            [
                p
                for p in markdown_files(REPO_ROOT)
                if p.parts[:3] == (".github", "scripts", "fixtures")
            ],
        )

    def test_historical_qualifier_stays_scoped(self):
        import re

        qualified = []
        for rel in markdown_files(REPO_ROOT):
            try:
                text = (REPO_ROOT / rel).read_text(encoding="utf-8")
            except (OSError, UnicodeError):
                continue
            if re.search(r"examined.head", text, re.IGNORECASE):
                qualified.append(rel.as_posix())
        self.assertEqual(["docs/api-delivery-map.md"], qualified)


if __name__ == "__main__":
    unittest.main()
