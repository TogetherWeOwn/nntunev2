"""Pin the e2e-lint browser image and execute its preflight with offline doubles.

The image replaces per-job apt/browser installation, not browser tests. A
missing executable, missing shared library, or failed close must stop the job
before helper execution. The real preflight and helper suite still run in CI.
"""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

from test_ci_pnpm_retry import split_jobs, split_steps, step_field

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/ci.yml"
VERIFY_NAME = "Verify preinstalled Playwright chromium (browser-level self-tests)"


def run_body(step):
    lines = step.splitlines()
    start = lines.index("        run: |") + 1
    return "\n".join(line[10:] for line in lines[start:] if line.strip())


def browser_step(workflow):
    return next(
        step for step in split_steps(split_jobs(workflow)["e2e-lint"])
        if step_field(step, "name") == VERIFY_NAME
    )


class TestPlaywrightImage(unittest.TestCase):
    def setUp(self):
        self.workflow = WORKFLOW.read_text()
        self.job = split_jobs(self.workflow)["e2e-lint"]

    def test_image_is_digest_pinned_and_matches_locked_playwright(self):
        package = json.loads((ROOT / "e2e/package.json").read_text())
        lock = json.loads((ROOT / "e2e/package-lock.json").read_text())
        version = package["devDependencies"]["@playwright/test"]
        self.assertEqual(lock["packages"]["node_modules/@playwright/test"]["version"], version)
        self.assertEqual(lock["packages"]["node_modules/playwright-core"]["version"], version)
        image = re.search(r"^    container: (\S+)\s*$", self.job, re.MULTILINE)
        self.assertIsNotNone(image, "e2e-lint needs a preinstalled browser image")
        self.assertRegex(
            image.group(1),
            rf"^mcr\.microsoft\.com/playwright:v{re.escape(version)}-noble@sha256:[0-9a-f]{{64}}$",
        )
        self.assertEqual(step_field(self.job, "runs-on"), "ubuntu-latest")
        self.assertEqual(step_field(self.job, "timeout-minutes"), "15")

    def test_preflight_and_original_helpers_are_required(self):
        steps = split_steps(self.job)
        verify = browser_step(self.workflow)
        self.assertEqual(step_field(verify, "working-directory"), "e2e")
        self.assertEqual(step_field(verify, "shell"), "bash")
        self.assertEqual(step_field(verify, "timeout-minutes"), "1")
        for step in (verify, next(s for s in steps if step_field(s, "run") == "npm run test:unit")):
            self.assertIsNone(step_field(step, "continue-on-error"))
            self.assertIsNone(step_field(step, "if"))
        self.assertLess(steps.index(verify), next(i for i, s in enumerate(steps) if step_field(s, "run") == "npm run test:unit"))
        self.assertNotIn("playwright install", self.job)
        self.assertNotIn("apt-get", self.job)
        self.assertIn("npm run typecheck", self.job)
        self.assertIn("npm run e2e:list", self.job)
        self.assertIn("node fixtures/two-track-self-check.mjs", self.job)
        aggregate = split_jobs(self.workflow)["ci-ok"]
        self.assertIn("e2e-lint", step_field(aggregate, "needs"))
        self.assertEqual(step_field(aggregate, "if"), "${{ always() }}")
        self.assertIn('all(.[]; .result == "success")', aggregate)
        self.assertIn("python3 .github/scripts/test_ci_playwright_image.py", self.workflow)

    def run_preflight(self, source=None):
        # Execute the exact workflow shell/Node command, replacing only the
        # installed package. No apt, browser download, network or privilege.
        with tempfile.TemporaryDirectory(dir=os.environ.get("PAPERCLIP_RUN_SCRATCH_DIR")) as tmp:
            root = Path(tmp)
            if source is not None:
                module = root / "node_modules/@playwright/test"
                module.mkdir(parents=True)
                (module / "package.json").write_text(json.dumps({
                    "name": "@playwright/test", "type": "module", "exports": "./index.js",
                }))
                (module / "index.js").write_text(source)
            env = dict(os.environ)
            env.pop("BASH_ENV", None)
            return subprocess.run(
                ["bash", "--noprofile", "--norc", "-eo", "pipefail", "-c",
                 run_body(browser_step(self.workflow)) + "\nprintf 'HELPERS_REACHED\\n'"],
                cwd=root, env=env, text=True, capture_output=True, timeout=10,
            )

    def test_usable_browser_is_launched_and_closed_before_helpers(self):
        result = self.run_preflight("""
export const chromium = { async launch() {
  console.log('LAUNCHED');
  return { async close() { console.log('CLOSED'); } };
} };
""")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["LAUNCHED", "CLOSED", "HELPERS_REACHED"])

    def test_missing_executable_stops_before_helpers(self):
        result = self.run_preflight("""
export const chromium = { async launch() {
  throw new Error('Executable does not exist: image/package version mismatch');
} };
""")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Executable does not exist", result.stderr)
        self.assertNotIn("HELPERS_REACHED", result.stdout)

    def test_missing_system_dependency_stops_before_helpers(self):
        result = self.run_preflight("""
export const chromium = { async launch() {
  throw new Error('error while loading shared libraries: libglib-2.0.so.0');
} };
""")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("libglib-2.0.so.0", result.stderr)
        self.assertNotIn("HELPERS_REACHED", result.stdout)

    def test_failed_browser_close_stops_before_helpers(self):
        result = self.run_preflight("""
export const chromium = { async launch() {
  return { async close() { throw new Error('browser close failed'); } };
} };
""")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("browser close failed", result.stderr)
        self.assertNotIn("HELPERS_REACHED", result.stdout)

    def test_missing_package_stops_before_helpers(self):
        result = self.run_preflight()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ERR_MODULE_NOT_FOUND", result.stderr)
        self.assertNotIn("HELPERS_REACHED", result.stdout)

    def test_python_launcher_works_when_helpers_preserve_only_path(self):
        step = next(s for s in split_steps(self.job) if step_field(s, "name") ==
                    "Preserve Python runtime for isolated helper subprocesses")
        with tempfile.TemporaryDirectory(dir=os.environ.get("PAPERCLIP_RUN_SCRATCH_DIR")) as tmp:
            root = Path(tmp) / "path with space and 'quote"
            runtime = root / "python"
            binary = runtime / "bin/python3"
            binary.parent.mkdir(parents=True)
            # Model a setup-python interpreter whose shared library is only
            # discoverable through its exported LD_LIBRARY_PATH. The existing
            # helper deliberately forwards PATH alone to subprocesses.
            binary.write_text("#!/bin/bash\n" +
                              'test "${LD_LIBRARY_PATH-}" = ' +
                              "\"$(dirname \"$0\")/../lib\" || exit 127\n" +
                              "printf 'PYTHON_READY\\n'\n")
            binary.chmod(0o755)
            runner = root / "runner"
            runner.mkdir()
            env = {"PATH": str(binary.parent) + ":/usr/bin:/bin",
                   "pythonLocation": str(runtime / "bin/.."),
                   "RUNNER_TEMP": str(runner), "GITHUB_PATH": str(root / "github-path")}
            control = subprocess.run([str(binary)], env={"PATH": env["PATH"]},
                                     text=True, capture_output=True, timeout=10)
            self.assertEqual(control.returncode, 127)
            provision = subprocess.run(["bash", "-eo", "pipefail", "-c", run_body(step)],
                                       env=env, text=True, capture_output=True, timeout=10)
            self.assertEqual(provision.returncode, 0, provision.stderr)
            tools = runner / "e2e-tools"
            self.assertEqual((root / "github-path").read_text().strip(), str(tools))
            result = subprocess.run([str(tools / "python3")], env={"PATH": env["PATH"]},
                                    text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, "PYTHON_READY\n")

    def test_jq_provisioning_is_pinned_bounded_and_required(self):
        step = next(s for s in split_steps(self.job) if step_field(s, "name") ==
                    "Install pinned standalone jq for shell-contract helpers")
        body = run_body(step)
        self.assertIn("https://github.com/jqlang/jq/releases/download/jq-1.8.2/jq-linux-amd64", body)
        self.assertIn("b1c22172dd303f3be49e935aa56aa48a8b7a46e0bc838b4997d3bb451495870f", body)
        self.assertIn("--connect-timeout 10 --max-time 30 --retry 3", body)
        self.assertIn("sha256sum -c -", body)
        self.assertIsNone(step_field(step, "continue-on-error"))
        self.assertIsNone(step_field(step, "if"))


if __name__ == "__main__":
    unittest.main()
