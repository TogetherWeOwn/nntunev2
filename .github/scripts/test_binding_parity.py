"""Unit tests for binding_parity.

Fixture repos under fixtures/binding-parity/ are shared with the CI job:
``match`` must pass, ``drift-missing`` and ``drift-skew`` must fail. The
remaining cases build throwaway mini-repos in a temp directory.
"""

import contextlib
import io
import os
import re
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

### KV namespaces (created 2026-10-13)

| Namespace | ID | Env |
|---|---|---|
| `demo-staging` | `fx-kv-staging` | staging |
| `demo-prod` | `fx-kv-prod` | prod |

### Hyperdrive bindings

| Config | ID | Intended binding |
|---|---|---|
| `demo-staging` | `fx-hd-staging` | staging app |
| `demo-prod` | `fx-hd-prod` | prod app |
| `demo-mb` | `fx-hd-mb` | shared mirror |

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
        inventory, ids = parse_ops_inventory(OPS_MD)
        self.assertEqual({"fx-audio-staging"}, inventory["staging"])
        self.assertEqual({"fx-audio-prod"}, inventory["prod"])
        self.assertIn("fx-hd-staging", ids["staging"]["hyperdrive"])
        self.assertNotIn("fx-hd-prod", ids["staging"]["hyperdrive"])
        self.assertIn("fx-hd-prod", ids["prod"]["hyperdrive"])

    def test_matrix_columns_follow_reordered_header(self):
        ops = OPS_MD.replace(
            "| Concern | staging | prod |", "| Concern | prod | staging |"
        ).replace(
            "| R2 audio | `fx-audio-staging` | `fx-audio-prod` |",
            "| R2 audio | `fx-audio-prod` | `fx-audio-staging` |",
        )
        inventory, _ = parse_ops_inventory(ops)
        self.assertEqual({"fx-audio-staging"}, inventory["staging"])
        self.assertEqual({"fx-audio-prod"}, inventory["prod"])

    def test_matrix_missing_environment_column_fails_closed(self):
        ops = OPS_MD.replace("| Concern | staging | prod |", "| Concern | staging | live |")
        with self.assertRaisesRegex(ParityError, "lacks staging/prod columns"):
            parse_ops_inventory(ops)

    def test_missing_matrix_fails_closed(self):
        with self.assertRaises(ParityError):
            parse_ops_inventory("# ops\n\nno matrix here\n")

    def test_missing_id_section_fails_closed(self):
        ops_without_kv = OPS_MD.replace(
            "### KV namespaces (created 2026-10-13)\n\n"
            "| Namespace | ID | Env |\n|---|---|---|\n"
            "| `demo-staging` | `fx-kv-staging` | staging |\n"
            "| `demo-prod` | `fx-kv-prod` | prod |\n\n",
            "",
        )
        with self.assertRaisesRegex(ParityError, "no '### KV namespaces' section"):
            parse_ops_inventory(ops_without_kv)

    def test_real_ops_kv_heading_and_environment_rows_are_parsed(self):
        ops_text = (REPO_ROOT / "docs" / "ops.md").read_text()
        _, ids = parse_ops_inventory(ops_text)
        section_match = re.search(
            r"^### KV namespaces[^\n]*\n(?P<section>.*?)(?=^#{2,3} |\Z)",
            ops_text,
            re.MULTILINE | re.DOTALL,
        )
        self.assertIsNotNone(section_match)
        table_rows = [
            [cell.strip() for cell in line.split("|")]
            for line in section_match.group("section").splitlines()
            if line.lstrip().startswith("|")
        ]
        header = [cell.lower() for cell in table_rows[0]]
        env_index = header.index("env")
        id_index = next(
            i for i, name in enumerate(header)
            if name == "id" or name.startswith("id /")
        )
        aliases = {"staging": "staging", "prod": "prod", "production": "prod"}
        rows = []
        for cells in table_rows[1:]:
            if all(not cell or re.fullmatch(r":?-{3,}:?", cell) for cell in cells):
                continue
            env_cell = cells[env_index].lower()
            envs = {
                env
                for alias, env in aliases.items()
                if re.search(rf"(?<![a-z0-9]){alias}(?![a-z0-9])", env_cell)
            }
            values = [
                value
                for value in re.findall(r"`([^`]+)`", cells[id_index])
                if not re.fullmatch(r"[A-Z][A-Z0-9]*_[A-Z0-9_]*", value)
            ]
            self.assertEqual(1, len(envs))
            self.assertEqual(1, len(values))
            rows.append((values[0], envs.pop()))
        self.assertTrue(rows)
        for resource_id, env in rows:
            other_env = "prod" if env == "staging" else "staging"
            self.assertIn(resource_id, ids[env]["kv"])
            self.assertNotIn(resource_id, ids[other_env]["kv"])

    def test_kv_environment_uses_header_columns(self):
        ops = OPS_MD.replace(
            "| Namespace | ID | Env |", "| Env | Namespace | ID / variable | Notes |"
        ).replace(
            "| `demo-staging` | `fx-kv-staging` | staging |",
            "| staging | `demo-staging` | `fx-kv-staging` / `KV_NAMESPACE_STAGING` | note |",
        ).replace(
            "| `demo-prod` | `fx-kv-prod` | prod |",
            "| prod | `demo-prod` | `fx-kv-prod` / `KV_NAMESPACE_PROD` | note |",
        )
        _, ids = parse_ops_inventory(ops)
        self.assertIn("fx-kv-staging", ids["staging"]["kv"])
        self.assertNotIn("fx-kv-staging", ids["prod"]["kv"])

    def test_kv_ids_preserve_case_and_production_alias(self):
        ops = OPS_MD.replace("fx-kv-staging", "FX-KV-STAGING").replace(
            "`fx-kv-prod` | prod |", "`fx-kv-prod` | production |"
        )
        _, ids = parse_ops_inventory(ops)
        self.assertIn("FX-KV-STAGING", ids["staging"]["kv"])
        self.assertIn("fx-kv-prod", ids["prod"]["kv"])
        self.assertNotIn("fx-kv-prod", ids["staging"]["kv"])

    def test_kv_unknown_environment_fails_closed(self):
        ops = OPS_MD.replace(
            "| `demo-staging` | `fx-kv-staging` | staging |",
            "| `demo-staging` | `fx-kv-staging` | stging |",
        )
        with self.assertRaisesRegex(
            ParityError, "unrecognized or ambiguous KV inventory environment"
        ):
            parse_ops_inventory(ops)

    def test_kv_inventory_requires_one_id_per_row(self):
        ops = OPS_MD.replace(
            "| `demo-staging` | `fx-kv-staging` | staging |",
            "| `demo-staging` | `fx-kv-staging` (`old id`) | staging |",
        )
        with self.assertRaisesRegex(ParityError, "KV inventory row lacks a unique id"):
            parse_ops_inventory(ops)

    def test_hyperdrive_mb_match_requires_a_delimited_config_name(self):
        for config in ("combo-drive", "team_mb"):
            ops = OPS_MD.replace("`demo-staging`", f"`{config}`")
            with self.assertRaisesRegex(ParityError, "unrecognized Hyperdrive inventory config"):
                parse_ops_inventory(ops)

    def test_hyperdrive_id_column_ignores_variable_token_order(self):
        ops = OPS_MD.replace(
            "`fx-hd-staging` | staging app",
            "`HYPERDRIVE_ID_STAGING` / `fx-hd-staging` | staging app",
        )
        _, ids = parse_ops_inventory(ops)
        self.assertIn("fx-hd-staging", ids["staging"]["hyperdrive"])
        self.assertNotIn("HYPERDRIVE_ID_STAGING", ids["staging"]["hyperdrive"])

    def test_hyperdrive_inventory_uses_only_id_column(self):
        ops = OPS_MD.replace(
            "| `demo-staging` | `fx-hd-staging` | staging app |",
            "| `demo-staging` | `fx-hd-staging` | `main` staging app |",
        )
        _, ids = parse_ops_inventory(ops)
        self.assertNotIn("main", ids["staging"]["hyperdrive"])
        self.assertNotIn("main", ids["prod"]["hyperdrive"])

    def test_shared_mb_config_allows_parenthetical_notes(self):
        for config in ("`demo-mb` (shared mirror)", "`demo-mb (shared mirror)`"):
            with self.subTest(config=config):
                ops = OPS_MD.replace("`demo-mb`", config)
                _, ids = parse_ops_inventory(ops)
                self.assertIn("fx-hd-mb", ids["staging"]["hyperdrive"])
                self.assertIn("fx-hd-mb", ids["prod"]["hyperdrive"])

    def test_hyperdrive_production_alias_maps_to_prod(self):
        ops = OPS_MD.replace("`demo-prod`", "`demo-production`")
        _, ids = parse_ops_inventory(ops)
        self.assertIn("fx-hd-prod", ids["prod"]["hyperdrive"])
        self.assertNotIn("fx-hd-prod", ids["staging"]["hyperdrive"])


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
            '\n[[env.staging.kv_namespaces]]\nbinding = "SURPRISE"\nid = "fx-kv"\n'
        )
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(make_repo(tmp, wrangler=wrangler))
            joined = "\n".join(drifts)
            self.assertIn("binding=SURPRISE", joined)
            self.assertIn("not in the manifest", joined)

    def test_unexpected_active_top_level_binding_is_drift(self):
        wrangler = MATCHING_WRANGLER + (
            '\n[[kv_namespaces]]\nbinding = "SURPRISE_TOP"\nid = "fx-kv"\n'
        )
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(make_repo(tmp, wrangler=wrangler))
            joined = "\n".join(drifts)
            self.assertIn("worker=demo env=- binding=SURPRISE_TOP", joined)
            self.assertIn("top-level binding is not in the manifest", joined)

    def test_top_level_resource_is_checked_against_prod_manifest(self):
        # Default wrangler deploy uses the root-level config, which is the
        # production target even when [env.production] is also configured.
        wrangler = MATCHING_WRANGLER + (
            '\n[[r2_buckets]]\nbinding = "AUDIO_CACHE"\n'
            'bucket_name = "fx-audio-staging"\n'
        )
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(make_repo(tmp, wrangler=wrangler))
            joined = "\n".join(drifts)
            self.assertIn("worker=demo env=prod binding=AUDIO_CACHE", joined)
            self.assertIn("fx-audio-staging", joined)
            self.assertIn("ops inventory 'fx-audio-prod'", joined)

    def test_unmapped_config_file_is_drift(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp)
            orphan = root / "workers" / "orphan"
            orphan.mkdir()
            (orphan / "wrangler.toml").write_text('name = "orphan"\n')
            drifts, _, _ = run_check(root)
            self.assertIn("workers/orphan/wrangler.toml", "\n".join(drifts))

    def test_unmapped_jsonc_config_is_drift(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp)
            orphan = root / "workers" / "orphan"
            orphan.mkdir()
            (orphan / "wrangler.jsonc").write_text('{"name":"orphan"}')
            drifts, _, _ = run_check(root)
            self.assertIn("workers/orphan/wrangler.jsonc", "\n".join(drifts))

    def test_unmapped_non_config_named_wrangler_file_is_ignored(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp)
            note = root / "workers" / "demo" / "wrangler.md"
            note.write_text("Worker configuration notes\\n")
            drifts, _, _ = run_check(root)
            self.assertEqual([], drifts)

    def test_mapped_jsonc_config_fails_closed_as_unsupported(self):
        manifest = MANIFEST.replace("wrangler.toml", "wrangler.jsonc")
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaisesRegex(ParityError, "TOML is required"):
                run_check(
                    make_repo(
                        tmp,
                        wrangler='{"name":"demo"}',
                        manifest=manifest,
                        config_name="wrangler.jsonc",
                    )
                )

    def test_duplicate_environment_aliases_fail_closed(self):
        wrangler = MATCHING_WRANGLER + "\n[env.prod]\n"
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaisesRegex(ParityError, "duplicate environment aliases") as raised:
                run_check(make_repo(tmp, wrangler=wrangler))
            self.assertEqual(
                1, str(raised.exception).count("workers/demo/wrangler.toml")
            )

    def test_real_config_does_not_inherit_top_level_bindings(self):
        wrangler = (
            'name = "demo"\n'
            '[[r2_buckets]]\nbinding = "AUDIO_CACHE"\nbucket_name = "fx-audio-staging"\n'
            '[[hyperdrive]]\nbinding = "DB"\nid = "fx-hd-staging"\n'
            '[env.staging]\n'
            '[[env.staging.r2_buckets]]\nbinding = "AUDIO_CACHE"\n'
            'bucket_name = "fx-audio-staging"\n'
        )
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(make_repo(tmp, wrangler=wrangler))
            joined = "\n".join(drifts)
            self.assertIn("env=staging binding=DB", joined)
            self.assertIn("env=prod binding=AUDIO_CACHE", joined)
            self.assertIn("env=prod binding=DB", joined)

    def test_id_inventory_is_environment_scoped(self):
        swapped = MANIFEST.replace(
            'staging = "fx-hd-staging"\nprod = "fx-hd-prod"',
            'staging = "fx-hd-prod"\nprod = "fx-hd-staging"',
        )
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(make_repo(tmp, manifest=swapped))
            joined = "\n".join(drifts)
            self.assertIn("env=staging binding=DB", joined)
            self.assertIn("env=prod binding=DB", joined)
            self.assertIn("absent from the ops inventory for staging", joined)
            self.assertIn("absent from the ops inventory for prod", joined)

    def test_id_inventory_is_binding_type_scoped(self):
        manifest = MANIFEST.replace(
            'staging = "fx-hd-staging"', 'staging = "fx-kv-staging"'
        )
        wrangler = MATCHING_WRANGLER.replace(
            'id = "fx-hd-staging"', 'id = "fx-kv-staging"'
        )
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(
                make_repo(tmp, wrangler=wrangler, manifest=manifest)
            )
            self.assertIn(
                "absent from the ops inventory for staging", "\n".join(drifts)
            )

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

        # A suppression is bound to the exact actual value as well as the
        # worker, env, binding and expected value.
        different_actual = manifest.replace(
            'actual = "fx-old-bucket"', 'actual = "fx-another-bucket"', 1
        )
        with tempfile.TemporaryDirectory() as tmp:
            drifts, warnings, _ = run_check(
                make_repo(tmp, wrangler=wrangler, manifest=different_actual)
            )
            joined = "\n".join(drifts)
            self.assertIn("resource 'fx-old-bucket' != ops inventory", joined)
            self.assertIn("stale known-drift entry", joined)
            self.assertEqual(1, len(warnings))

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
            '\n[[env.staging.vectorize]]\nbinding = "SEARCH"\nindex_name = "fx-idx"\n'
        )
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(make_repo(tmp, wrangler=wrangler))
            joined = "\n".join(drifts)
            self.assertIn("binding=SEARCH", joined)
            self.assertIn("unknown:vectorize", joined)

    def test_name_keyed_unknown_binding_tables_are_flagged(self):
        configs = {
            "send_email": (
                '\n[[env.staging.send_email]]\nname = "EMAIL"\n'
                'allowed_sender_addresses = ["noreply@example.com"]\n',
                "EMAIL",
            ),
            "unsafe.bindings": (
                '\n[[env.staging.unsafe.bindings]]\nname = "LEGACY"\n'
                'type = "text_blob"\n',
                "LEGACY",
            ),
            "ratelimits": (
                '\n[[env.staging.ratelimits]]\nname = "RL"\n'
                'namespace_id = "1234"\n',
                "RL",
            ),
            "logfwdr.bindings": (
                '\n[[env.staging.logfwdr.bindings]]\nname = "LOG"\n'
                'destination = "syslog"\n',
                "LOG",
            ),
        }
        for table, (addition, binding) in configs.items():
            with self.subTest(table=table), tempfile.TemporaryDirectory() as tmp:
                drifts, _, _ = run_check(
                    make_repo(tmp, wrangler=MATCHING_WRANGLER + addition)
                )
                joined = "\n".join(drifts)
                self.assertIn("binding=" + binding, joined)
                self.assertIn("unknown:" + table, joined)
                self.assertIn("staging/prod skew", joined)

    def test_name_keyed_unknown_binding_without_name_fails_with_exit_two(self):
        wrangler = MATCHING_WRANGLER + "\n[[env.staging.send_email]]\n"
        self.assert_wrangler_fails_closed_with_exit_two(wrangler)

    def test_name_keyed_malformed_entries_fail_closed_with_exit_two(self):
        # A name-keyed binding table that is present but not a table (or a
        # list entry that is not a table) is malformed, not absent: it must
        # fail closed instead of being silently ignored.
        scalar_cases = {
            "send_email": MATCHING_WRANGLER.replace(
                "[env.staging]\n", '[env.staging]\nsend_email = "oops"\n', 1
            ),
            "ratelimits": MATCHING_WRANGLER.replace(
                "[env.staging]\n", "[env.staging]\nratelimits = 123\n", 1
            ),
            "unsafe.bindings": MATCHING_WRANGLER
            + '\n[env.staging.unsafe]\nbindings = "oops"\n',
            "logfwdr.bindings": MATCHING_WRANGLER
            + "\n[env.staging.logfwdr]\nbindings = 123\n",
        }
        for table, wrangler in scalar_cases.items():
            with self.subTest(table=table, kind="scalar"):
                self.assert_wrangler_fails_closed_with_exit_two(wrangler)
        list_cases = {
            "send_email": MATCHING_WRANGLER.replace(
                "[env.staging]\n",
                '[env.staging]\nsend_email = ["oops"]\n',
                1,
            ),
            "ratelimits": MATCHING_WRANGLER.replace(
                "[env.staging]\n", "[env.staging]\nratelimits = [123]\n", 1
            ),
            "unsafe.bindings": MATCHING_WRANGLER
            + '\n[env.staging.unsafe]\nbindings = ["oops"]\n',
            "logfwdr.bindings": MATCHING_WRANGLER
            + "\n[env.staging.logfwdr]\nbindings = [123]\n",
        }
        for table, wrangler in list_cases.items():
            with self.subTest(table=table, kind="list-entry"):
                self.assert_wrangler_fails_closed_with_exit_two(wrangler)

    def test_manifest_key_typo_fails_closed(self):
        manifest = MANIFEST.replace('staging = "fx-audio-staging"',
                                     'stagign = "fx-audio-staging"')
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(ParityError):
                run_check(make_repo(tmp, manifest=manifest))

    def test_non_string_resource_fails_closed_with_exit_two(self):
        wrangler = MATCHING_WRANGLER.replace(
            'bucket_name = "fx-audio-staging"', "bucket_name = 123"
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, wrangler=wrangler)
            with self.assertRaisesRegex(ParityError, "bucket_name must be a non-empty string"):
                run_check(root)
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(2, main(["--root", str(root)]))
            self.assertIn("::error::binding-parity:", output.getvalue())
            self.assertNotIn("Traceback", output.getvalue())

    def test_empty_resource_fails_closed_with_exit_two_for_presence_only_binding(self):
        wrangler = MATCHING_WRANGLER.replace(
            '[[env.staging.hyperdrive]]',
            '[[env.staging.kv_namespaces]]\nbinding = "STATE"\nid = ""\n'
            '[[env.staging.hyperdrive]]',
        ).replace(
            '[[env.production.hyperdrive]]',
            '[[env.production.kv_namespaces]]\nbinding = "STATE"\nid = "fx-state-prod"\n'
            '[[env.production.hyperdrive]]',
        )
        manifest = MANIFEST + '\n[[worker.binding]]\nbinding = "STATE"\ntype = "kv"\n'
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, wrangler=wrangler, manifest=manifest)
            with self.assertRaisesRegex(ParityError, "id must be a non-empty string"):
                run_check(root)
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(2, main(["--root", str(root)]))
            self.assertIn("::error::binding-parity:", output.getvalue())
            self.assertNotIn("Traceback", output.getvalue())

    def assert_wrangler_fails_closed_with_exit_two(self, wrangler):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, wrangler=wrangler)
            with self.assertRaises(ParityError):
                run_check(root)
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(2, main(["--root", str(root)]))
            self.assertIn("::error::binding-parity:", output.getvalue())
            self.assertNotIn("Traceback", output.getvalue())

    def test_present_assets_or_unknown_binding_must_be_non_empty_string(self):
        for binding_value in ('""', "0"):
            with self.subTest(table="assets", binding_value=binding_value):
                self.assert_wrangler_fails_closed_with_exit_two(
                    MATCHING_WRANGLER
                    + f"\n[assets]\nbinding = {binding_value}\n"
                )
            with self.subTest(table="unknown", binding_value=binding_value):
                self.assert_wrangler_fails_closed_with_exit_two(
                    MATCHING_WRANGLER
                    + "\n[[env.staging.vectorize]]\n"
                    + f"binding = {binding_value}\nindex_name = \"fx-index\"\n"
                )

    def test_bindingless_assets_and_unknown_table_remain_ignored(self):
        wrangler = MATCHING_WRANGLER + (
            '\n[assets]\ndirectory = "./public"\n'
            '\n[[env.staging.vectorize]]\nindex_name = "fx-index"\n'
        )
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(make_repo(tmp, wrangler=wrangler))
        self.assertEqual([], drifts)

    def test_non_string_queue_consumer_fails_closed_with_exit_two(self):
        wrangler = MATCHING_WRANGLER + (
            '\n[env.staging.queues]\n'
            '[[env.staging.queues.consumers]]\nqueue = 123\n'
        )
        manifest = MANIFEST + '''
[[worker.binding]]
binding = "consumer:fx-jobs"
type = "queue_consumer"
staging = "fx-jobs"
prod = "fx-jobs"
'''
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, wrangler=wrangler, manifest=manifest)
            with self.assertRaisesRegex(ParityError, "queue.*must be a string"):
                run_check(root)
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(2, main(["--root", str(root)]))
            self.assertIn("::error::binding-parity:", output.getvalue())
            self.assertNotIn("Traceback", output.getvalue())

    def test_non_string_binding_fails_closed_with_exit_two(self):
        wrangler = MATCHING_WRANGLER.replace(
            "[[env.staging.hyperdrive]]",
            '[[env.staging.r2_buckets]]\nbinding = 123\n'
            'bucket_name = "fx-extra"\n[[env.staging.hyperdrive]]',
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, wrangler=wrangler)
            with self.assertRaisesRegex(ParityError, "must be a string"):
                run_check(root)
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(2, main(["--root", str(root)]))
            self.assertIn("::error::binding-parity:", output.getvalue())
            self.assertNotIn("Traceback", output.getvalue())

    def assert_manifest_fails_closed(self, manifest, pattern):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, manifest=manifest)
            with self.assertRaisesRegex(ParityError, pattern):
                run_check(root)
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(2, main(["--root", str(root)]))
            self.assertIn("::error::binding-parity:", output.getvalue())
            self.assertNotIn("Traceback", output.getvalue())

    def test_manifest_non_string_binding_name_fails_closed_with_exit_two(self):
        self.assert_manifest_fails_closed(
            MANIFEST.replace('binding = "AUDIO_CACHE"', "binding = 123"),
            "binding must be a non-empty string",
        )

    def test_manifest_malformed_binding_fields_fail_closed_with_exit_two(self):
        cases = {
            "type is an array": (
                'type = "r2"', 'type = ["r2"]', "type must be a non-empty string"
            ),
            "type is an int": (
                'type = "r2"', "type = 7", "type must be a non-empty string"
            ),
            "type is unknown": (
                'type = "r2"', 'type = "nope"', "unknown type"
            ),
            "staging is an array": (
                'staging = "fx-audio-staging"', 'staging = ["fx-audio-staging"]',
                "staging must be a non-empty string",
            ),
            "prod is an int": (
                'prod = "fx-audio-prod"', "prod = 5",
                "prod must be a non-empty string",
            ),
            "config is an int": (
                'config = "workers/demo/wrangler.toml"', "config = 123",
                "config must be a non-empty string",
            ),
            "worker name is an array": (
                'name = "demo"', 'name = ["demo"]', "name must be a non-empty string"
            ),
        }
        for label, (old, new, pattern) in cases.items():
            with self.subTest(label):
                self.assert_manifest_fails_closed(MANIFEST.replace(old, new, 1), pattern)

    def test_manifest_non_table_arrays_fail_closed_with_exit_two(self):
        self.assert_manifest_fails_closed(
            'worker = "demo"\n', r"\[\[worker\]\] must be an array of tables"
        )
        self.assert_manifest_fails_closed(
            '[[worker]]\nname = "demo"\nconfig = "workers/demo/wrangler.toml"\n'
            '[worker.binding]\nbinding = "AUDIO_CACHE"\ntype = "r2"\n',
            r"\[\[worker.binding\]\] must be an array of tables",
        )
        self.assert_manifest_fails_closed(
            'known_drift = "x"\n' + MANIFEST,
            r"\[\[known_drift\]\] must be an array of tables",
        )

    def test_manifest_malformed_known_drift_fields_fail_closed_with_exit_two(self):
        base = {
            "worker": '"demo"',
            "env": '"staging"',
            "binding": '"AUDIO_CACHE"',
            "actual": '"fx-old-bucket"',
            "expected": '"fx-audio-staging"',
            "reason": '"fixture: value fix owned elsewhere"',
        }
        cases = {
            "worker": "123",
            "env": '["staging"]',
            "binding": "[1]",
            "actual": '["fx-old-bucket"]',
            "expected": "5",
            "reason": "true",
        }
        for key, bad in cases.items():
            with self.subTest(key):
                fields = dict(base, **{key: bad})
                manifest = MANIFEST + "\n[[known_drift]]\n" + "".join(
                    f"{k} = {v}\n" for k, v in fields.items()
                )
                self.assert_manifest_fails_closed(
                    manifest, f"{key} must be a non-empty string"
                )
        with self.subTest("missing actual"):
            fields = {k: v for k, v in base.items() if k != "actual"}
            manifest = MANIFEST + "\n[[known_drift]]\n" + "".join(
                f"{k} = {v}\n" for k, v in fields.items()
            )
            self.assert_manifest_fails_closed(
                manifest, "actual must be a non-empty string"
            )
        with self.subTest("unknown env"):
            fields = dict(base, env='"qa"')
            manifest = MANIFEST + "\n[[known_drift]]\n" + "".join(
                f"{k} = {v}\n" for k, v in fields.items()
            )
            self.assert_manifest_fails_closed(manifest, "env must be one of")

    def test_unparsable_config_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, wrangler="name = [not toml")
            with self.assertRaises(ParityError):
                run_check(root)
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(2, main(["--root", str(root)]))


if __name__ == "__main__":
    unittest.main()
