"""Pin unrecognized Wrangler environment-section drift.

Covers ``binding_parity.py`` ``parse_config`` unknown-env collection
(:322-330) and the ``check`` drift arm (:645-648): an ``[env.<name>]``
section whose name is not a staging/production alias must surface as
``worker=<name> env=<raw> ... unrecognized environment section`` with
exit 1 (drift), never exit 2 (fail-closed) or a clean pass.

Fixture helpers are imported from :mod:`test_binding_parity` without
modifying that suite; all configs are throwaway mini-repos in temp dirs.
"""

import contextlib
import io
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from binding_parity import main, parse_config
from test_binding_parity import MATCHING_WRANGLER, make_repo, run_check

PREVIEW_WITH_BINDINGS = MATCHING_WRANGLER + (
    "\n[env.preview]\n"
    '[[env.preview.r2_buckets]]\nbinding = "AUDIO_CACHE"\n'
    'bucket_name = "fx-audio-preview"\n'
)

PREVIEW_WITHOUT_BINDINGS = MATCHING_WRANGLER + "\n[env.preview]\n"

PROD_ALIAS_WRANGLER = MATCHING_WRANGLER.replace("env.production", "env.prod")


def run_main(root):
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        code = main(["--root", str(root)])
    return code, out.getvalue()


class TestUnknownEnvironments(unittest.TestCase):
    def test_parse_config_returns_unknown_env_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, wrangler=PREVIEW_WITH_BINDINGS)
            config = root / "workers" / "demo" / "wrangler.toml"
            effective, unknown, _ = parse_config(config)
            self.assertIn("preview", unknown)
            # Unknown sections never feed the staging/prod effective sets.
            self.assertEqual({"staging", "prod"}, set(effective))
            resources = {
                item["resource"]
                for env_bindings in effective.values()
                for item in env_bindings.values()
            }
            self.assertNotIn("fx-audio-preview", resources)

    def test_unknown_env_with_bindings_is_drift_exit_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, wrangler=PREVIEW_WITH_BINDINGS)
            drifts, _, _ = run_check(root)
            joined = "\n".join(drifts)
            self.assertIn("worker=demo env=preview", joined)
            self.assertIn("unrecognized environment section", joined)
            code, output = run_main(root)
            self.assertEqual(1, code)
            self.assertIn("worker=demo env=preview", output)
            self.assertIn("unrecognized environment section", output)
            self.assertNotIn("Traceback", output)

    def test_unknown_env_without_bindings_is_drift_exit_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, wrangler=PREVIEW_WITHOUT_BINDINGS)
            drifts, _, _ = run_check(root)
            joined = "\n".join(drifts)
            self.assertIn("worker=demo env=preview", joined)
            self.assertIn("unrecognized environment section", joined)
            code, output = run_main(root)
            self.assertEqual(1, code)
            self.assertIn("unrecognized environment section", output)
            self.assertNotIn("Traceback", output)

    def test_recognized_aliases_only_pass(self):
        for wrangler in (MATCHING_WRANGLER, PROD_ALIAS_WRANGLER):
            with self.subTest(wrangler="production" if wrangler is MATCHING_WRANGLER else "prod"):
                with tempfile.TemporaryDirectory() as tmp:
                    root = make_repo(tmp, wrangler=wrangler)
                    drifts, warnings, _ = run_check(root)
                    self.assertEqual([], drifts)
                    self.assertEqual([], warnings)
                    code, _ = run_main(root)
                    self.assertEqual(0, code)


if __name__ == "__main__":
    unittest.main()
