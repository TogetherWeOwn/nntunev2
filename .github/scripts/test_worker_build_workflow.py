"""Regression tests for canonical Worker build and upload sequencing."""

import importlib.util
from pathlib import Path
import re
import tempfile
import tomllib
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("check_worker_upload", Path(__file__).with_name("check_worker_upload.py"))
UPLOAD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(UPLOAD)

ROOT = Path(__file__).resolve().parents[2]
BUILD = "worker-build --release -- --locked"


def job(text, name):
    match = re.search(rf"^  {re.escape(name)}:\n(.*?)(?=^  [\w-]+:|\Z)", text, re.M | re.S)
    if not match:
        raise AssertionError(f"missing job {name}")
    return match.group(1)


def steps(section):
    return re.findall(r"^      - .*?(?=^      - |\Z)", section, re.M | re.S)


class WorkerBuildWorkflowTests(unittest.TestCase):
    def test_upload_fixture_preserves_complete_build_tree(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            source = base / "source"
            build = source / "workers/api/build"
            (build / "worker").mkdir(parents=True)
            files = {
                "worker/shim.mjs": b"export * from '../index.js';\n",
                "index.js": b"import wasm from './index_bg.wasm';\n",
                "index_bg.wasm": b"\x00asm\x01\x00\x00\x00",
            }
            for name, content in files.items():
                (build / name).write_bytes(content)
            manifest = source / UPLOAD.MANIFEST
            manifest.parent.mkdir(parents=True)
            manifest.write_text("worker = []\n")
            with patch.object(UPLOAD, "ROOT", source):
                tree, wdir = UPLOAD.fixture(base, "api", "fixture")
            self.assertEqual(wdir, tree / "workers/api")
            self.assertEqual((tree / UPLOAD.MANIFEST).read_bytes(), manifest.read_bytes())
            for name, content in files.items():
                self.assertEqual((wdir / "build" / name).read_bytes(), content)

    def test_staging_prepares_and_builds_before_upload(self):
        text = (ROOT / ".github/workflows/deploy.yml").read_text()
        self.assertNotIn("cargo install", text)
        for worker in ("api", "mb", "ingest"):
            with self.subTest(worker=worker):
                section = job(text, f"staging-{worker}")
                items = steps(section)
                install = next(i for i, s in enumerate(items) if "worker_tools.py install" in s)
                build = next(i for i, s in enumerate(items) if f"Build {worker} with canonical tools" in s)
                upload = next(i for i, s in enumerate(items) if "id: deploy\n" in s)
                first_credentials = next(i for i, s in enumerate(items) if "secrets.CLOUDFLARE_" in s)
                self.assertLess(install, build)
                self.assertLess(build, first_credentials)
                self.assertLess(build, upload)
                for step in (items[install], items[build]):
                    self.assertNotIn("secrets.", step)
                    self.assertNotIn("env:", step)
                    self.assertIn("if: steps.config.outputs.configured == 'true'", step)
                    if worker == "ingest":
                        self.assertIn("steps.config.outputs.enabled == 'true'", step)
                self.assertNotIn("    env:\n", section.split("    steps:\n", 1)[0])
                self.assertIn(f"working-directory: workers/{worker}", items[build])
                self.assertIn("set -euo pipefail", items[build])
                self.assertIn(BUILD + ' 2>&1 | tee', items[build])
                self.assertLess(items[build].index('cargo metadata --locked --format-version 1'),
                                items[build].index(BUILD))
                self.assertGreater(items[build].index('git diff --exit-code -- Cargo.lock'),
                                   items[build].index(BUILD))
                self.assertIn('worker_tools.py check-build "$RUNNER_TEMP/worker-', items[build])
                self.assertIn("command: deploy --env staging --var BUILD_SHA:", items[upload])
                generate = next(s for s in items if "name: Generate staging wrangler.toml" in s)
                self.assertNotIn("[build]", generate)
                self.assertNotIn("[env.staging.build]", generate)
                self.assertNotIn("worker-build", generate)
                self.assertNotIn("--no-build", items[upload])

    def test_committed_configs_keep_the_local_build_command(self):
        for path in ("workers/api/wrangler.toml.example", "workers/mb/wrangler.toml",
                     "workers/ingest/wrangler.toml"):
            with self.subTest(path=path):
                config = tomllib.loads((ROOT / path).read_text())
                self.assertEqual(config["build"]["command"], BUILD)

    def test_release_builds_are_required_and_credential_free(self):
        text = (ROOT / ".github/workflows/ci.yml").read_text()
        section = job(text, "worker-release-builds")
        for forbidden in ("secrets.", "vars.", "environment:", "wrangler deploy", "--no-opt"):
            self.assertNotIn(forbidden, section)
        self.assertIn("permissions:\n      contents: read", section)
        self.assertIn("persist-credentials: false", section)
        self.assertIn("python3 -I .github/scripts/test_worker_tools.py", section)
        self.assertIn("python3 -I .github/scripts/test_worker_build_workflow.py", section)
        self.assertIn("for worker in api mb ingest; do", section)
        self.assertIn(BUILD, section)
        self.assertLess(section.index('cargo metadata --locked --format-version 1'), section.index(BUILD))
        self.assertGreater(section.index('git diff --exit-code -- Cargo.lock'), section.index(BUILD))
        self.assertIn("worker_tools.py check-build", section)
        self.assertIn("set -euo pipefail", section)
        self.assertIn("worker-release-builds", job(text, "ci-ok"))
        self.assertIn("pnpm install --frozen-lockfile --ignore-scripts", section)
        self.assertIn("--config.minimumReleaseAge=4320", section)
        self.assertIn("check_worker_upload.py --wrangler", section)
        self.assertLess(section.index(BUILD), section.index("check_worker_upload.py --wrangler"))


if __name__ == "__main__":
    unittest.main()
