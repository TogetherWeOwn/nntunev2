"""Unit tests for binding_parity.

Fixture repos under fixtures/binding-parity/ are shared with the CI job:
``match`` must pass, ``drift-missing`` and ``drift-skew`` must fail. The
remaining cases build throwaway mini-repos in a temp directory.
"""

import contextlib
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from binding_parity import (
    ParityError,
    check,
    extract_commented_bindings,
    main,
    parse_ops_inventory,
)

SCRIPTS = Path(__file__).resolve().parent
FIXTURES = SCRIPTS / "fixtures" / "binding-parity"
REPO_ROOT = SCRIPTS.parents[1]

OPS_MD = """# fixture ops

Hyperdrive ids: `fx-hd-staging`, `fx-hd-prod`.

## Environments matrix

| Concern | staging | prod |
|---|---|---|
| R2 audio | `fx-audio-staging` | `fx-audio-prod` |
"""

MANIFEST = """[[worker]]
name = "demo"
config = "workers/demo/wrangler.toml"

[[worker.binding]]
binding = "AUDIO_CACHE"
type = "r2"
staging = "fx-audio-staging"
prod = "fx-audio-prod"

[[worker.binding]]
binding = "DB"
type = "hyperdrive"
staging = "fx-hd-staging"
prod = "fx-hd-prod"
"""

MATCHING_WRANGLER = """name = "demo"
main = "shim.mjs"
compatibility_date = "2026-10-01"

[env.staging]
[[env.staging.r2_buckets]]
binding = "AUDIO_CACHE"
bucket_name = "fx-audio-staging"
[[env.staging.hyperdrive]]
binding = "DB"
id = "fx-hd-staging"

[env.production]
[[env.production.r2_buckets]]
binding = "AUDIO_CACHE"
bucket_name = "fx-audio-prod"
[[env.production.hyperdrive]]
binding = "DB"
id = "fx-hd-prod"
"""


def make_repo(tmp, wrangler=MATCHING_WRANGLER, manifest=MANIFEST, ops=OPS_MD,
              config_name="wrangler.toml"):
    root = Path(tmp)
    (root / ".github" / "scripts").mkdir(parents=True)
    (root / "docs").mkdir()
    (root / "workers" / "demo").mkdir(parents=True)
    (root / ".github" / "scripts" / "binding_manifest.toml").write_text(manifest)
    (root / "docs" / "ops.md").write_text(ops)
    (root / "workers" / "demo" / config_name).write_text(wrangler)
    return root


def run_check(root):
    return check(
        root,
        root / ".github" / "scripts" / "binding_manifest.toml",
        root / "docs" / "ops.md",
    )


class TestFixtureRepos(unittest.TestCase):
    def test_match_fixture_passes(self):
        root = FIXTURES / "match"
        drifts, warnings, _ = run_check(root)
        self.assertEqual([], drifts)
        self.assertEqual([], warnings)
        self.assertEqual(0, main(["--root", str(root)]))

    def test_drift_missing_fixture_fails_and_names_binding(self):
        drifts, _, _ = run_check(FIXTURES / "drift-missing")
        self.assertTrue(drifts)
        joined = "\n".join(drifts)
        self.assertIn("worker=demo", joined)
        self.assertIn("binding=STATE", joined)
        self.assertIn("env=staging", joined)
        self.assertIn("env=prod", joined)
        self.assertIn("missing required binding", joined)

    def test_drift_skew_fixture_fails_and_names_skew(self):
        drifts, _, _ = run_check(FIXTURES / "drift-skew")
        joined = "\n".join(drifts)
        self.assertIn("binding=JOBS", joined)
        self.assertIn("staging/prod skew", joined)

    def test_main_returns_red_on_drift_fixture(self):
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(1, main(["--root", str(FIXTURES / "drift-skew")]))

    def test_real_repo_is_in_parity(self):
        drifts, _, worker_count = run_check(REPO_ROOT)
        self.assertEqual([], drifts)
        self.assertGreaterEqual(worker_count, 5)


class TestOpsInventoryParsing(unittest.TestCase):
    def test_matrix_columns_map_to_envs(self):
        inventory, tokens = parse_ops_inventory(OPS_MD)
        self.assertEqual({"fx-audio-staging"}, inventory["staging"])
        self.assertEqual({"fx-audio-prod"}, inventory["prod"])
        self.assertIn("fx-hd-staging", tokens)

    def test_missing_matrix_fails_closed(self):
        with self.assertRaises(ParityError):
            parse_ops_inventory("# ops\n\nno matrix here\n")


class TestCommentedExamples(unittest.TestCase):
    def test_commented_block_counts_as_declared(self):
        text = (
            '# [[hyperdrive]]\n# binding = "DB"\n# id = "<fill in>"\n\n'
            "# Secrets live elsewhere; never here.\n"
        )
        found = extract_commented_bindings(text)
        entry = found[("hyperdrive", "DB")]
        self.assertTrue(entry["declared"])
        self.assertEqual("<fill in>", entry["resource"])

    def test_prose_comments_do_not_open_blocks(self):
        text = '# binding = "NOPE" is how a binding looks\n'
        self.assertEqual({}, extract_commented_bindings(text))

    def test_example_template_with_placeholders_passes(self):
        # Bindings declared only as commented blocks with placeholders:
        # presence counts, resource comparison is skipped in templates.
        wrangler = (
            'name = "demo"\nmain = "shim.mjs"\ncompatibility_date = "2026-10-01"\n\n'
            '# [[r2_buckets]]\n# binding = "AUDIO_CACHE"\n'
            '# bucket_name = "<bucket>"\n\n'
            '# [[hyperdrive]]\n# binding = "DB"\n# id = "<id>"\n'
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, wrangler=wrangler,
                             config_name="wrangler.toml.example")
            manifest = (root / ".github" / "scripts" / "binding_manifest.toml")
            manifest.write_text(
                MANIFEST.replace("wrangler.toml", "wrangler.toml.example")
            )
            drifts, _, _ = run_check(root)
            self.assertEqual([], drifts)

    def test_placeholder_in_real_toml_is_drift(self):
        wrangler = MATCHING_WRANGLER.replace("fx-audio-staging", "<bucket>")
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(make_repo(tmp, wrangler=wrangler))
            joined = "\n".join(drifts)
            self.assertIn("binding=AUDIO_CACHE", joined)
            self.assertIn("placeholder", joined)


class TestDriftCases(unittest.TestCase):
    def test_matching_mini_repo_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            drifts, warnings, _ = run_check(make_repo(tmp))
            self.assertEqual([], drifts)
            self.assertEqual([], warnings)

    def test_resource_name_drift_names_worker_env_binding(self):
        wrangler = MATCHING_WRANGLER.replace("fx-audio-prod", "fx-audio-wrong")
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(make_repo(tmp, wrangler=wrangler))
            joined = "\n".join(drifts)
            self.assertIn("worker=demo env=prod binding=AUDIO_CACHE", joined)
            self.assertIn("fx-audio-wrong", joined)
            self.assertNotIn("env=staging", joined)

    def test_manifest_resource_absent_from_ops_inventory(self):
        manifest = MANIFEST.replace("fx-audio-staging", "fx-not-provisioned")
        wrangler = MATCHING_WRANGLER.replace("fx-audio-staging", "fx-not-provisioned")
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(
                make_repo(tmp, wrangler=wrangler, manifest=manifest)
            )
            joined = "\n".join(drifts)
            self.assertIn("binding=AUDIO_CACHE", joined)
            self.assertIn("absent from the ops inventory for staging", joined)

    def test_unexpected_active_binding_is_drift(self):
        wrangler = MATCHING_WRANGLER + (
            '\n[[kv_namespaces]]\nbinding = "SURPRISE"\nid = "fx-kv"\n'
        )
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(make_repo(tmp, wrangler=wrangler))
            joined = "\n".join(drifts)
            self.assertIn("binding=SURPRISE", joined)
            self.assertIn("not in the manifest", joined)

    def test_unmapped_config_file_is_drift(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp)
            orphan = root / "workers" / "orphan"
            orphan.mkdir()
            (orphan / "wrangler.toml").write_text('name = "orphan"\n')
            drifts, _, _ = run_check(root)
            self.assertIn("workers/orphan/wrangler.toml", "\n".join(drifts))

    def test_id_mismatch_never_prints_id_values(self):
        sentinel = "sentinel-id-value-must-not-print"
        wrangler = MATCHING_WRANGLER.replace("fx-hd-prod", sentinel)
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, wrangler=wrangler)
            drifts, _, _ = run_check(root)
            joined = "\n".join(drifts)
            self.assertIn("binding=DB", joined)
            self.assertIn("does not match the ops inventory", joined)
            self.assertNotIn(sentinel, joined)
            self.assertNotIn("fx-hd-prod", joined)
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(1, main(["--root", str(root)]))
            self.assertNotIn(sentinel, out.getvalue())

    def test_known_drift_suppresses_exact_match_only(self):
        wrangler = MATCHING_WRANGLER.replace(
            "fx-audio-staging", "fx-old-bucket"
        ).replace("fx-audio-prod", "fx-old-bucket")
        manifest = MANIFEST + """
[[known_drift]]
worker = "demo"
env = "staging"
binding = "AUDIO_CACHE"
actual = "fx-old-bucket"
expected = "fx-audio-staging"
reason = "fixture: value fix owned elsewhere"

[[known_drift]]
worker = "demo"
env = "prod"
binding = "AUDIO_CACHE"
actual = "fx-old-bucket"
expected = "fx-audio-prod"
reason = "fixture: value fix owned elsewhere"
"""
        with tempfile.TemporaryDirectory() as tmp:
            drifts, warnings, _ = run_check(
                make_repo(tmp, wrangler=wrangler, manifest=manifest)
            )
            self.assertEqual([], drifts)
            self.assertEqual(2, len(warnings))

    def test_stale_known_drift_fails(self):
        # Config now matches the inventory, so the known-drift entries
        # suppress nothing and must be removed.
        manifest = MANIFEST + """
[[known_drift]]
worker = "demo"
env = "staging"
binding = "AUDIO_CACHE"
actual = "fx-old-bucket"
expected = "fx-audio-staging"
reason = "fixture: value fix owned elsewhere"
"""
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(make_repo(tmp, manifest=manifest))
            joined = "\n".join(drifts)
            self.assertIn("stale known-drift", joined)
            self.assertIn("binding=AUDIO_CACHE", joined)

    def test_duplicate_binding_fails_closed(self):
        wrangler = MATCHING_WRANGLER + (
            '\n[[kv_namespaces]]\nbinding = "STATE"\nid = "a"\n'
            '[[kv_namespaces]]\nbinding = "STATE"\nid = "b"\n'
        )
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(ParityError):
                run_check(make_repo(tmp, wrangler=wrangler))

    def test_unknown_binding_table_is_flagged_not_ignored(self):
        wrangler = MATCHING_WRANGLER + (
            '\n[[vectorize]]\nbinding = "SEARCH"\nindex_name = "fx-idx"\n'
        )
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(make_repo(tmp, wrangler=wrangler))
            joined = "\n".join(drifts)
            self.assertIn("binding=SEARCH", joined)
            self.assertIn("unknown:vectorize", joined)

    def test_manifest_key_typo_fails_closed(self):
        manifest = MANIFEST.replace('staging = "fx-audio-staging"',
                                     'stagign = "fx-audio-staging"')
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(ParityError):
                run_check(make_repo(tmp, manifest=manifest))

    def test_unparsable_config_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, wrangler="name = [not toml")
            with self.assertRaises(ParityError):
                run_check(root)
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(2, main(["--root", str(root)]))


if __name__ == "__main__":
    unittest.main()
