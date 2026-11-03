"""Unit tests for the manifest `absent_in` mechanism in binding_parity.

A binding with `absent_in = ["staging"]` (or `"prod"`) must be missing in
the listed environments and present in the others; the staging/prod skew
check ignores exactly those declared absences. Every direction is pinned:
declaring the binding where it must be absent is drift, omitting it where
it must be present is drift, and undeclared skew still fails. Malformed
`absent_in` values fail closed with ParityError (exit 2), never silently.
"""

import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from binding_parity import ParityError, check, load_manifest

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
| Producer queue | `fx-queue-staging` | `fx-queue-prod` |
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
binding = "REMOTE"
type = "service"
absent_in = ["staging"]

[[worker.binding]]
binding = "consumer:fx-queue-staging"
type = "queue_consumer"
staging = "fx-queue-staging"
absent_in = ["prod"]

[[worker.binding]]
binding = "consumer:fx-queue-prod"
type = "queue_consumer"
prod = "fx-queue-prod"
absent_in = ["staging"]
"""

MATCHING_WRANGLER = """name = "demo"
main = "shim.mjs"
compatibility_date = "2026-10-01"

[vars]
AUDIO_BUCKET_NAME = "fx-audio-prod"

[[r2_buckets]]
binding = "AUDIO_CACHE"
bucket_name = "fx-audio-prod"

[[services]]
binding = "REMOTE"
service = "fx-remote-prod"

[[queues.consumers]]
queue = "fx-queue-prod"
max_batch_size = 10
max_retries = 4
dead_letter_queue = "fx-queue-prod"

[env.staging]
[[env.staging.r2_buckets]]
binding = "AUDIO_CACHE"
bucket_name = "fx-audio-staging"
[[env.staging.queues.consumers]]
queue = "fx-queue-staging"
max_batch_size = 10
max_retries = 4
dead_letter_queue = "fx-queue-staging"

[env.production]
[[env.production.r2_buckets]]
binding = "AUDIO_CACHE"
bucket_name = "fx-audio-prod"
[[env.production.services]]
binding = "REMOTE"
service = "fx-remote-prod"
[[env.production.queues.consumers]]
queue = "fx-queue-prod"
max_batch_size = 10
max_retries = 4
dead_letter_queue = "fx-queue-prod"
"""


def make_repo(tmp, wrangler=MATCHING_WRANGLER, manifest=MANIFEST):
    root = Path(tmp)
    (root / ".github" / "scripts").mkdir(parents=True)
    (root / "docs").mkdir()
    (root / "workers" / "demo").mkdir(parents=True)
    (root / ".github" / "scripts" / "binding_manifest.toml").write_text(manifest)
    (root / "docs" / "ops.md").write_text(OPS_MD)
    (root / "workers" / "demo" / "wrangler.toml").write_text(wrangler)
    return root


def run_check(root):
    return check(
        root,
        root / ".github" / "scripts" / "binding_manifest.toml",
        root / "docs" / "ops.md",
    )


class TestAbsentIn(unittest.TestCase):
    def test_declared_absences_pass_with_no_skew(self):
        with tempfile.TemporaryDirectory() as tmp:
            drifts, warnings, count = run_check(make_repo(tmp))
        self.assertEqual(drifts, [])
        self.assertEqual(warnings, [])
        self.assertEqual(count, 1)

    def test_declaring_an_absent_binding_is_drift(self):
        wrangler = MATCHING_WRANGLER.replace(
            "[[env.staging.r2_buckets]]",
            "[[env.staging.services]]\nbinding = \"REMOTE\"\nservice = \"fx-remote-staging\"\n\n[[env.staging.r2_buckets]]",
            1,
        )
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(make_repo(tmp, wrangler=wrangler))
        self.assertEqual(len(drifts), 1)
        self.assertIn("requires it to be absent here (absent_in)", drifts[0])
        self.assertIn("env=staging", drifts[0])
        self.assertIn("REMOTE", drifts[0])

    def test_omitting_a_required_binding_is_still_drift(self):
        wrangler = MATCHING_WRANGLER.replace(
            '[[env.production.services]]\nbinding = "REMOTE"\nservice = "fx-remote-prod"\n',
            "",
        )
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(make_repo(tmp, wrangler=wrangler))
        self.assertTrue(
            any("missing required binding" in drift and "REMOTE" in drift for drift in drifts),
            f"expected a missing-binding drift for REMOTE, got {drifts}",
        )

    def test_undeclared_skew_still_fails(self):
        # Drop the staging AUDIO_CACHE binding without declaring an absence:
        # presence/parity must fail, not pass silently.
        wrangler = MATCHING_WRANGLER.replace(
            '[[env.staging.r2_buckets]]\nbinding = "AUDIO_CACHE"\nbucket_name = "fx-audio-staging"\n',
            "",
        )
        with tempfile.TemporaryDirectory() as tmp:
            drifts, _, _ = run_check(make_repo(tmp, wrangler=wrangler))
        self.assertTrue(
            any("missing required binding" in drift for drift in drifts),
            f"expected a missing-binding drift, got {drifts}",
        )
        self.assertTrue(
            any("staging/prod skew" in drift for drift in drifts),
            f"expected a skew drift, got {drifts}",
        )

    def test_absent_in_with_expected_resource_fails_closed(self):
        manifest = MANIFEST.replace(
            'binding = "REMOTE"\ntype = "service"\nabsent_in = ["staging"]',
            'binding = "REMOTE"\ntype = "service"\nstaging = "fx-remote-staging"\nabsent_in = ["staging"]',
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, manifest=manifest)
            with self.assertRaises(ParityError):
                load_manifest(root / ".github" / "scripts" / "binding_manifest.toml")

    def test_absent_in_unknown_environment_fails_closed(self):
        manifest = MANIFEST.replace('absent_in = ["staging"]', 'absent_in = ["dev"]')
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, manifest=manifest)
            with self.assertRaises(ParityError):
                load_manifest(root / ".github" / "scripts" / "binding_manifest.toml")

    def test_absent_in_all_environments_fails_closed(self):
        manifest = MANIFEST.replace(
            'absent_in = ["staging"]', 'absent_in = ["staging", "prod"]', 1
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, manifest=manifest)
            with self.assertRaises(ParityError):
                load_manifest(root / ".github" / "scripts" / "binding_manifest.toml")


if __name__ == "__main__":
    unittest.main()
