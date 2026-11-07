"""Negative controls for the qualification-only tool installer and CI boundary."""

import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("qualification_tools", Path(__file__).with_name("qualification_tools.py"))
TOOLS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TOOLS)


class QualificationToolsTests(unittest.TestCase):
    def test_changed_archive_refuses(self):
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            TOOLS.verified(b"changed", "0" * 64)

    def test_installer_refuses_outside_actions(self):
        with patch.dict(os.environ, {"GITHUB_ACTIONS": "false"}):
            with self.assertRaisesRegex(RuntimeError, "Actions-only"):
                TOOLS.install()

    def test_build_checker_requires_every_override_and_an_empty_cache(self):
        with tempfile.TemporaryDirectory() as raw:
            temp = Path(raw)
            cache = temp / "cache"
            with patch.dict(os.environ, {"RUNNER_TEMP": raw, "XDG_CACHE_HOME": str(cache)}):
                lines = [f"Using custom tool from {name}: /verified/tool\n" for name in (
                    "WASM_BINDGEN_BIN", "ESBUILD_BIN", "WASM_OPT_BIN",
                )]
                for name in ("api", "ingest", "composite"):
                    (temp / f"qualification-{name}.log").write_text("".join(lines))
                TOOLS.check_builds()
                (temp / "qualification-api.log").write_text("".join(lines[:-1]))
                with self.assertRaisesRegex(ValueError, "missing verified override"):
                    TOOLS.check_builds()
                (temp / "qualification-api.log").write_text("".join(lines) + "Downloading tool\n")
                with self.assertRaisesRegex(ValueError, "attempted a tool download"):
                    TOOLS.check_builds()
                (temp / "qualification-api.log").write_text("".join(lines))
                (cache / "worker-build").mkdir(parents=True)
                (cache / "worker-build/unverified").touch()
                with self.assertRaisesRegex(ValueError, "unverified tool"):
                    TOOLS.check_builds()

    def test_job_is_secretless_and_required(self):
        text = (ROOT / ".github/workflows/ci.yml").read_text()
        section = text.split("  worker-preparation-qualification:\n", 1)[1].split("  ci-ok:\n", 1)[0]
        for forbidden in ("secrets.", "vars.", "environment:", "wrangler deploy", "--no-opt"):
            self.assertNotIn(forbidden, section)
        self.assertIn("permissions:\n      contents: read", section)
        self.assertIn("persist-credentials: false", section)
        self.assertIn("pnpm install --frozen-lockfile --ignore-scripts", section)
        self.assertIn("worker-build --release -- --locked --features preparation-service", section)
        self.assertIn("worker-build --release -- --locked 2>&1", section)
        self.assertIn("mv build build-ordinary", section)
        self.assertIn("worker-build --release -- --locked --features preparation-qualification", section)
        self.assertIn("bash workers/api/tests/run_migration_tests.sh", section)
        # Synthetic producer bridges must never enter an effective build.
        for config in ROOT.glob("workers/*/wrangler*.toml"):
            self.assertNotIn("preparation-qualification", config.read_text())
        ingest = (ROOT / "workers/ingest/src/lib.rs").read_text()
        self.assertIn('#[cfg(all(target_arch = "wasm32", feature = "preparation-qualification"))]\nmod qualification;', ingest)
        self.assertIn("set -euo pipefail", section)
        self.assertIn("qualification_tools.py check-builds", section)
        self.assertIn("worker-preparation-qualification", text.split("  ci-ok:\n", 1)[1])


if __name__ == "__main__":
    unittest.main()
