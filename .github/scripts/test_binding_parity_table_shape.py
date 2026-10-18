"""Pin binding-parity malformed table-shape exit 2.

Covers both halves of the ``except (AttributeError, TypeError)`` guard in
``binding_parity.parse_config``: well-formed TOML whose shapes are not
tables (``env`` as a string or list trips ``.items()`` -> AttributeError;
top-level ``r2_buckets`` as an int trips ``for item in 5`` -> TypeError)
must fail closed with ``malformed wrangler table shape`` and exit 2 via
``main``, never exit 1 and never an uncaught traceback. A valid-config
control proves the harness itself passes.

Temporary mini-repo configs only; no fixture changes. Manifest and ops
fixtures carry synthetic resource ids, and the failure output is
asserted to never repeat them.
"""

import contextlib
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from binding_parity import ParityError, check, main

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

CONFIG_HEAD = """name = "demo"
main = "shim.mjs"
compatibility_date = "2026-10-01"
"""

ENV_STRING_WRANGLER = CONFIG_HEAD + 'env = "staging"\n'

ENV_LIST_WRANGLER = CONFIG_HEAD + 'env = ["staging"]\n'

# ``r2_buckets`` as an int trips the TypeError half of the guard
# (``for item in data.get("r2_buckets", [])`` with an int is not iterable),
# where the ``env`` non-table shapes below trip the AttributeError half
# (``.items()`` on a str/list).
R2_INT_WRANGLER = CONFIG_HEAD + "r2_buckets = 5\n"


def make_repo(tmp, wrangler=MATCHING_WRANGLER):
    root = Path(tmp)
    (root / ".github" / "scripts").mkdir(parents=True)
    (root / "docs").mkdir()
    (root / "workers" / "demo").mkdir(parents=True)
    (root / ".github" / "scripts" / "binding_manifest.toml").write_text(MANIFEST)
    (root / "docs" / "ops.md").write_text(OPS_MD)
    (root / "workers" / "demo" / "wrangler.toml").write_text(wrangler)
    return root


def run_check(root):
    return check(
        root,
        root / ".github" / "scripts" / "binding_manifest.toml",
        root / "docs" / "ops.md",
    )


class TestMalformedTableShape(unittest.TestCase):
    """Well-formed TOML with non-table shapes fails closed via exit 2."""

    def assert_malformed_shape_fails_closed(self, wrangler, expect_cause):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, wrangler=wrangler)
            with self.assertRaisesRegex(
                ParityError, r"malformed wrangler table shape"
            ) as ctx:
                run_check(root)
            # Pin which half of the ``except (AttributeError, TypeError)``
            # tuple fired, so narrowing the guard either way goes red.
            self.assertIsInstance(ctx.exception.__cause__, expect_cause)
            stdout = io.StringIO()
            stderr = io.StringIO()
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(
                stderr
            ):
                code = main(["--root", str(root)])
            self.assertEqual(2, code)
            self.assertNotEqual(1, code)
            combined = stdout.getvalue() + stderr.getvalue()
            self.assertIn("::error::binding-parity:", stdout.getvalue())
            self.assertIn("malformed wrangler table shape", combined)
            self.assertNotIn("Traceback", combined)
            # Synthetic resource ids from the manifest/ops fixtures must
            # never leak into the failure annotation.
            self.assertNotIn("fx-", combined)

    def test_env_string_malformed_shape_exits_two(self):
        self.assert_malformed_shape_fails_closed(
            ENV_STRING_WRANGLER, AttributeError
        )

    def test_env_list_malformed_shape_exits_two(self):
        self.assert_malformed_shape_fails_closed(ENV_LIST_WRANGLER, AttributeError)

    def test_r2_int_malformed_shape_exits_two(self):
        self.assert_malformed_shape_fails_closed(R2_INT_WRANGLER, TypeError)

    def test_valid_config_control_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp)
            drifts, warnings, _ = run_check(root)
            self.assertEqual([], drifts)
            self.assertEqual([], warnings)
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(0, main(["--root", str(root)]))


if __name__ == "__main__":
    unittest.main()
