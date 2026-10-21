"""Pin binding-parity known_drift allowlist entry guards.

Covers the four ``load_manifest`` known_drift validation guards in
``binding_parity.py``: a ``[[known_drift]]`` entry naming an unmapped worker, an
unmapped binding, an id-typed (KV/Hyperdrive) binding, or an ``expected``
value that disagrees with the binding map must each fail closed with
``ParityError``. Without these, a rogue allowlist entry could legitimize
real staging/prod divergence. A consumed-entry control proves a
well-formed entry still suppresses exactly one mismatch.

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

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from binding_parity import ParityError, main
from test_binding_parity import MATCHING_WRANGLER, OPS_MD, make_repo, run_check

MANIFEST_WITH_KV = """[[worker]]
name = "demo"
config = "workers/demo/wrangler.toml"

[[worker.binding]]
binding = "AUDIO_CACHE"
type = "r2"
staging = "fx-audio-staging"
prod = "fx-audio-prod"

[[worker.binding]]
binding = "SESSION_CACHE"
type = "kv"
staging = "fx-kv-staging"
prod = "fx-kv-prod"

[[worker.binding]]
binding = "DB"
type = "hyperdrive"
staging = "fx-hd-staging"
prod = "fx-hd-prod"
"""

WRANGLER_WITH_KV = MATCHING_WRANGLER + """
[[env.staging.kv_namespaces]]
binding = "SESSION_CACHE"
id = "fx-kv-staging"

[[env.production.kv_namespaces]]
binding = "SESSION_CACHE"
id = "fx-kv-prod"
"""

# Staging R2 value drifted away from the manifest; the control
# [[known_drift]] entry below names exactly this mismatch.
DRIFTED_WRANGLER = WRANGLER_WITH_KV.replace(
    'bucket_name = "fx-audio-staging"', 'bucket_name = "fx-audio-staging-drift"'
)

BASE_MANIFEST = MANIFEST_WITH_KV

KNOWN_DRIFT_TMPL = """
[[known_drift]]
worker = "{worker}"
env = "{env}"
binding = "{binding}"
actual = "{actual}"
expected = "{expected}"
reason = "fixture drift pinned by the known_drift guard suite"
"""


def manifest_with_drift(**overrides):
    entry = {
        "worker": "demo",
        "env": "staging",
        "binding": "AUDIO_CACHE",
        "actual": "fx-audio-staging",
        "expected": "fx-audio-staging",
    }
    entry.update(overrides)
    return BASE_MANIFEST + KNOWN_DRIFT_TMPL.format(**entry)


class TestKnownDriftGuards(unittest.TestCase):
    """Each malformed allowlist entry fails closed; a good one still works."""

    def assert_guard_fails_closed(self, manifest, pattern, wrangler=WRANGLER_WITH_KV):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(tmp, wrangler=wrangler, manifest=manifest)
            with self.assertRaisesRegex(ParityError, pattern):
                run_check(root)
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
            self.assertRegex(combined, pattern)
            self.assertNotIn("Traceback", combined)
            # Synthetic resource ids from the manifest/ops fixtures must
            # never leak into the failure annotation.
            self.assertNotIn("fx-", combined)

    def test_unknown_worker_fails_closed(self):
        self.assert_guard_fails_closed(
            manifest_with_drift(worker="ghost"),
            r"unmapped worker",
        )

    def test_unknown_binding_fails_closed(self):
        self.assert_guard_fails_closed(
            manifest_with_drift(binding="NOPE"),
            r"unmapped binding",
        )

    def test_id_typed_binding_fails_closed(self):
        # Both id-typed arms (KV SESSION_CACHE and Hyperdrive DB) must
        # fail closed. Expected agrees with the map in each case, isolating
        # the id-typed guard itself from the disagreeing-expected guard.
        for binding, ident in (
            ("SESSION_CACHE", "fx-kv-staging"),
            ("DB", "fx-hd-staging"),
        ):
            with self.subTest(binding=binding):
                self.assert_guard_fails_closed(
                    manifest_with_drift(
                        binding=binding,
                        actual=ident,
                        expected=ident,
                    ),
                    r"not allowed for id-typed bindings",
                )

    def test_disagreeing_expected_fails_closed(self):
        self.assert_guard_fails_closed(
            manifest_with_drift(expected="fx-audio-somewhere-else"),
            r"disagrees with the binding map",
        )

    def test_consumed_entry_still_suppresses_one_mismatch(self):
        manifest = manifest_with_drift(
            actual="fx-audio-staging-drift",
            expected="fx-audio-staging",
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = make_repo(
                tmp, wrangler=DRIFTED_WRANGLER, manifest=manifest, ops=OPS_MD
            )
            drifts, warnings, _ = run_check(root)
            self.assertEqual([], drifts)
            self.assertEqual(1, len(warnings))
            self.assertIn("known drift", warnings[0])
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(0, main(["--root", str(tmp)]))


if __name__ == "__main__":
    unittest.main()
