#!/usr/bin/env python3
"""Pin the expiry rule, offline ensure() API paths and staging upload guard.

Name, prefix, 14 days and enabled must hold on one rule before upload.

Run: python3 workers/mb/spike/test_lifecycle.py
"""

import contextlib
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import call, patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import lifecycle  # noqa: E402


def rule(**overrides) -> dict:
    out = lifecycle.desired_rule()
    out.update(overrides)
    return out


class RuleOk(unittest.TestCase):
    def test_desired_rule_passes(self):
        self.assertTrue(lifecycle.rule_ok({"rules": [rule()]}))

    def test_empty_or_missing_fails(self):
        self.assertFalse(lifecycle.rule_ok({}))
        self.assertFalse(lifecycle.rule_ok({"rules": []}))

    def test_other_expiry_fails(self):
        thirty = {"condition": {"type": "Age", "maxAge": 30 * 86400}}
        self.assertFalse(lifecycle.rule_ok({"rules": [rule(deleteObjectsTransition=thirty)]}))

    def test_date_expiry_fails(self):
        dated = {"condition": {"type": "Date", "date": "2027-01-01T00:00:00Z"}}
        self.assertFalse(lifecycle.rule_ok({"rules": [rule(deleteObjectsTransition=dated)]}))

    def test_no_expiry_action_fails(self):
        bare = rule()
        del bare["deleteObjectsTransition"]
        self.assertFalse(lifecycle.rule_ok({"rules": [bare]}))

    def test_disabled_fails(self):
        self.assertFalse(lifecycle.rule_ok({"rules": [rule(enabled=False)]}))

    def test_other_prefix_fails(self):
        self.assertFalse(lifecycle.rule_ok({"rules": [rule(conditions={"prefix": "other/"})]}))

    def test_name_and_prefix_split_across_rules_fails(self):
        split = [
            rule(conditions={"prefix": "other/"}),
            rule(id="something-else"),
        ]
        self.assertFalse(lifecycle.rule_ok({"rules": split}))


class Repaired(unittest.TestCase):
    def test_replaces_stale_rule_and_keeps_others(self):
        other = {"id": "abort-multipart", "enabled": True, "conditions": {"prefix": ""}}
        stale = rule(enabled=False)
        fixed = lifecycle.repaired({"rules": [other, stale]})
        self.assertEqual(fixed["rules"], [other, lifecycle.desired_rule()])
        self.assertTrue(lifecycle.rule_ok(fixed))


class Ensure(unittest.TestCase):
    def setUp(self):
        self.token = "synthetic-token"
        self.bucket = "synthetic-bucket"
        self.url = lifecycle.API.format(account="synthetic-account", bucket=self.bucket)
        self.env = patch.dict(os.environ, {
            "CLOUDFLARE_API_TOKEN": self.token,
            "CLOUDFLARE_ACCOUNT_ID": "synthetic-account",
        }, clear=True)
        self.env.start()
        self.addCleanup(self.env.stop)
        self.network = patch.object(lifecycle.urllib.request, "urlopen",
                                    side_effect=AssertionError("unexpected network call"))
        self.network.start()
        self.addCleanup(self.network.stop)

    def test_already_valid_only_reads(self):
        with patch.object(lifecycle, "call", return_value={"rules": [rule()]}) as api:
            self.assertEqual(lifecycle.ensure(self.bucket), 0)
        api.assert_called_once_with("GET", self.url, self.token)

    def test_repairs_then_reads_back(self):
        config = {"rules": [rule(enabled=False), {"id": "unrelated"}]}
        fixed = lifecycle.repaired(config)
        with patch.object(lifecycle, "call", side_effect=[config, {}, fixed]) as api:
            self.assertEqual(lifecycle.ensure(self.bucket), 0)
        self.assertEqual(api.call_args_list, [
            call("GET", self.url, self.token),
            call("PUT", self.url, self.token, fixed),
            call("GET", self.url, self.token),
        ])

    def test_bad_readback_fails(self):
        config = {"rules": []}
        with patch.object(lifecycle, "call", side_effect=[config, {}, config]) as api:
            with contextlib.redirect_stderr(io.StringIO()) as stderr:
                self.assertEqual(lifecycle.ensure(self.bucket), 1)
        self.assertIn("cannot confirm", stderr.getvalue())
        self.assertEqual(api.call_args_list, [
            call("GET", self.url, self.token),
            call("PUT", self.url, self.token, lifecycle.repaired(config)),
            call("GET", self.url, self.token),
        ])

    def test_unsuccessful_api_payload_raises_at_each_step(self):
        for step, method in enumerate(["GET", "PUT", "GET"]):
            with self.subTest(step=step, method=method):
                payloads = [{"success": True, "result": {"rules": []}}] * step
                payloads.append({"success": False, "errors": ["synthetic failure"]})
                responses = [io.BytesIO(json.dumps(p).encode()) for p in payloads]
                with patch.object(lifecycle.urllib.request, "urlopen",
                                  side_effect=responses) as api:
                    with self.assertRaisesRegex(RuntimeError, f"{method} lifecycle:"):
                        lifecycle.ensure(self.bucket)
                self.assertEqual(api.call_count, step + 1)
                self.assertEqual(api.call_args.args[0].method, method)


class StageUploadGuard(unittest.TestCase):
    def test_failed_ensure_prevents_object_uploads(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            spike = root / "spike"
            spike.mkdir()
            stage = spike / "stage.sh"
            shutil.copyfile(Path(__file__).with_name("stage.sh"), stage)
            snap = root / "snap"
            snap.mkdir()
            (snap / "recording.idx").touch()
            (snap / "recording.dat.000").touch()
            bin_dir = root / "bin"
            bin_dir.mkdir()
            for name, script in {
                "python3": '#!/bin/sh\nprintf "%s\\n" "$*" >> "$PYTHON_LOG"\nexit 1\n',
                "npx": '#!/bin/sh\nprintf "%s\\n" "$*" >> "$NPX_LOG"\n'
                       'case "$*" in *"r2 bucket info"*) exit 0;; *) exit 1;; esac\n',
            }.items():
                stub = bin_dir / name
                stub.write_text(script)
                stub.chmod(0o755)
            npx_log = root / "npx.log"
            python_log = root / "python.log"
            result = subprocess.run(["/bin/bash", str(stage), "up", str(snap)],
                                    env={
                                        "PATH": f"{bin_dir}:/usr/bin:/bin",
                                        "RUNNER_TEMP": str(root),
                                        "MB_DUMP": "synthetic-dump",
                                        "WRANGLER_VERSION": "0.0.0",
                                        "CLOUDFLARE_API_TOKEN": "synthetic-token",
                                        "CLOUDFLARE_ACCOUNT_ID": "synthetic-account",
                                        "NPX_LOG": str(npx_log),
                                        "PYTHON_LOG": str(python_log),
                                    }, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("not uploading", result.stderr)
            self.assertEqual(python_log.read_text().splitlines(), [
                f"{spike / 'lifecycle.py'} ensure nntunev2-mb-snap-staging",
            ])
            self.assertEqual(npx_log.read_text().splitlines(), [
                "--yes wrangler@0.0.0 r2 bucket info nntunev2-mb-snap-staging",
            ])
            self.assertNotIn("object put", npx_log.read_text())


class StageDeployGuard(unittest.TestCase):
    def test_failed_deploy_still_attempts_worker_teardown(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            spike = root / "spike"
            spike.mkdir()
            stage = spike / "stage.sh"
            shutil.copyfile(Path(__file__).with_name("stage.sh"), stage)
            snap = root / "snap"
            snap.mkdir()
            (snap / "artist.idx").touch()
            (snap / "artist.dat.000").touch()
            bin_dir = root / "bin"
            bin_dir.mkdir()
            for name, script in {
                "python3": "#!/bin/sh\nexit 0\n",
                "npx": '#!/bin/sh\nprintf "%s\\n" "$*" >> "$NPX_LOG"\n'
                       'case "$*" in *" deploy "*) exit 1;; *) exit 0;; esac\n',
            }.items():
                stub = bin_dir / name
                stub.write_text(script)
                stub.chmod(0o755)
            log = root / "npx.log"
            env = {"PATH": f"{bin_dir}:/usr/bin:/bin", "RUNNER_TEMP": str(root),
                   "MB_DUMP": "synthetic-dump", "WRANGLER_VERSION": "0.0.0", "NPX_LOG": str(log)}
            up = subprocess.run(["/bin/bash", str(stage), "up", str(snap)],
                                env=env, capture_output=True, text=True, timeout=10)
            self.assertNotEqual(up.returncode, 0)
            down = subprocess.run(["/bin/bash", str(stage), "down"],
                                  env=env, capture_output=True, text=True, timeout=10)
            self.assertEqual(down.returncode, 0, down.stderr)
            self.assertIn(" delete --name nntune-mb-snap-staging --force", log.read_text())


if __name__ == "__main__":
    unittest.main()
