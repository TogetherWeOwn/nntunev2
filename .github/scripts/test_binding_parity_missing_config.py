"""Pin missing mapped Wrangler configs as drift (exit 1, not exit 2).

A manifest-mapped config that is absent from an otherwise valid mini-repo
is drift: check() names the worker and the exact mapped-config path, emits
no warnings, keeps the expected worker count, and fabricates no per-binding
cascade. The CLI exits 1 (drift), never 2 (input/parser error), with no
traceback. A directory standing where the mapped config should be hits the
same missing-config guard.
"""

import contextlib
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from binding_parity import check, main

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

CONFIG_REL = "workers/demo/wrangler.toml"


def make_repo(tmp, wrangler=MATCHING_WRANGLER, manifest=MANIFEST, ops=OPS_MD):
    root = Path(tmp)
    (root / ".github" / "scripts").mkdir(parents=True)
    (root / "docs").mkdir()
    (root / "workers" / "demo").mkdir(parents=True)
    (root / ".github" / "scripts" / "binding_manifest.toml").write_text(manifest)
    (root / "docs" / "ops.md").write_text(ops)
    (root / CONFIG_REL).write_text(wrangler)
    return root


def run_check(root):
    return check(
        root,
        root / ".github" / "scripts" / "binding_manifest.toml",
        root / "docs" / "ops.md",
    )


def run_cli(root):
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        code = main(["--root", str(root)])
    return code, buf.getvalue()


class TestMissingMappedConfig(unittest.TestCase):
    def test_valid_mini_repo_passes_as_control(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp)
            drifts, warnings, worker_count = run_check(root)
            self.assertEqual([], drifts)
            self.assertEqual([], warnings)
            self.assertEqual(1, worker_count)
            code, output = run_cli(root)
            self.assertEqual(0, code, output)
            self.assertNotIn("Traceback", output)

    def test_missing_mapped_config_is_single_drift(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp)
            (root / CONFIG_REL).unlink()
            drifts, warnings, worker_count = run_check(root)
            self.assertEqual([], warnings)
            self.assertEqual(1, worker_count)
            self.assertEqual(1, len(drifts), drifts)
            drift = drifts[0]
            self.assertIn("worker=demo", drift)
            self.assertIn(CONFIG_REL, drift)
            self.assertIn("mapped config", drift)
            self.assertIn("is missing", drift)
            joined = "\n".join(drifts)
            for cascade in (
                "missing required binding",
                "staging/prod skew",
                "not in the manifest",
                "unrecognized environment",
                "ops inventory",
                "placeholder",
            ):
                self.assertNotIn(cascade, joined)

    def test_missing_config_cli_exits_one_not_two(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp)
            (root / CONFIG_REL).unlink()
            code, output = run_cli(root)
            self.assertEqual(1, code, output)
            self.assertIn("::error::binding-parity:", output)
            self.assertIn(CONFIG_REL, output)
            self.assertIn("worker=demo", output)
            self.assertNotIn("Traceback", output)

    def test_directory_at_config_hits_same_missing_guard(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp)
            config = root / CONFIG_REL
            config.unlink()
            config.mkdir()
            drifts, warnings, worker_count = run_check(root)
            self.assertEqual([], warnings)
            self.assertEqual(1, worker_count)
            self.assertEqual(1, len(drifts), drifts)
            self.assertIn(CONFIG_REL, drifts[0])
            self.assertIn("is missing", drifts[0])
            code, output = run_cli(root)
            self.assertEqual(1, code, output)
            self.assertNotIn("Traceback", output)


if __name__ == "__main__":
    unittest.main()
