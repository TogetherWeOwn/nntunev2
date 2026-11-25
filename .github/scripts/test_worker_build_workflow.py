"""Regression tests for canonical Worker build and upload sequencing."""

import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest.mock import patch
from uuid import uuid4

SPEC = importlib.util.spec_from_file_location("check_worker_upload", Path(__file__).with_name("check_worker_upload.py"))
UPLOAD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(UPLOAD)
CONFIG_SPEC = importlib.util.spec_from_file_location(
    "staging_stream_config", Path(__file__).with_name("staging_stream_config.py"),
)
STREAM_CONFIG = importlib.util.module_from_spec(CONFIG_SPEC)
CONFIG_SPEC.loader.exec_module(STREAM_CONFIG)

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
    def test_synthetic_invocation_record(self):
        probe_id = uuid4().hex
        clean_url = "https://staging.example.test/v1/plans"
        probe_url = clean_url + "?redaction_probe=synthetic-only"
        event = {
            "kind": "invocation",
            "worker": "nntune-api-staging",
            "probe_id": probe_id,
            "method": "GET",
            "url": clean_url,
        }
        command = [sys.executable, "-I", SPEC.origin, "--invocation-probe-id"]
        record = json.dumps(event).encode()

        def verify(raw, expected=probe_id):
            return subprocess.run([*command, expected], input=raw, capture_output=True, timeout=5)

        passed = verify(record)
        self.assertEqual((passed.returncode, passed.stdout, passed.stderr),
                         (0, b"check_worker_upload: synthetic invocation redaction ok\n", b""))

        for case, raw, diagnostic in (
            ("missing event", b"", "missing synthetic invocation record"),
            ("empty event", b"{}", "missing or malformed synthetic invocation record"),
            ("wrong Worker", json.dumps(event | {"worker": "synthetic-web-worker"}).encode(),
             "unexpected synthetic invocation Worker"),
            ("stale marker", json.dumps(event | {"probe_id": uuid4().hex}).encode(),
             "synthetic invocation does not match this probe"),
            ("missing URL", json.dumps(event | {"url": None}).encode(),
             "synthetic invocation URL is missing or differs from the redacted URL"),
            ("retained query", json.dumps(event | {"url": probe_url}).encode(),
             "synthetic invocation URL is missing or differs from the redacted URL"),
            ("query delimiter", json.dumps(event | {"url": clean_url + "?"}).encode(),
             "synthetic invocation URL is missing or differs from the redacted URL"),
            ("duplicate field", b'{"url":"synthetic-duplicate",' + record[1:],
             "synthetic invocation record has duplicate fields"),
            ("two records", record + b"\n" + record, "invalid synthetic invocation record JSON"),
            ("malformed", b'{"url":"synthetic-incomplete"', "invalid synthetic invocation record JSON"),
            ("oversized", record + b" " * 4096, "synthetic invocation record exceeds size limit"),
        ):
            with self.subTest(case=case):
                refused = verify(raw)
                self.assertEqual((refused.returncode, refused.stdout, refused.stderr),
                                 (1, b"", f"check_worker_upload: FAIL: {diagnostic}\n".encode()))

        invalid_marker = verify(json.dumps(event | {"probe_id": ""}).encode(), "")
        self.assertEqual((invalid_marker.returncode, invalid_marker.stdout, invalid_marker.stderr),
                         (1, b"", b"check_worker_upload: FAIL: invalid synthetic invocation probe ID\n"))

    def test_synthetic_invocation_rejects_wrong_rendered_worker(self):
        workflow = (ROOT / UPLOAD.WORKFLOW).read_text()
        manifest = {"worker": [{"name": "api", "binding": [{"binding": "HYPERDRIVE", "staging": "1" * 32}]}]}
        config = {"env": {"staging": {
            "name": "synthetic-web-worker",
            "compatibility_flags": ["enable_request_signal"],
            "observability": {"redact_query_string": True},
        }}}
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            with (
                patch.object(UPLOAD, "fixture", return_value=(base, base)),
                patch.object(UPLOAD, "generate", return_value=config),
                patch.object(UPLOAD, "dry_run") as upload,
            ):
                with self.assertRaisesRegex(UPLOAD.Failure, "unexpected staging API Worker"):
                    UPLOAD.scenario(None, workflow, "1" * 40, manifest, base, "", "api", False)
                upload.assert_not_called()

    def test_private_upload_uses_the_gate_and_checks_all_connections_before_deploy(self):
        text = (ROOT / ".github/workflows/deploy.yml").read_text()
        gate = job(text, "gate")
        self.assertIn("private_trial_upload: ${{ steps.trial.outputs.private_trial_upload || '0' }}", gate)
        items = steps(job(text, "staging-api"))
        build, generate, cache, deploy = (
            next(i for i, step in enumerate(items) if name in step)
            for name in ("Build api with canonical tools", "Generate staging wrangler.toml",
                         "Require uncached private trial connections", "id: deploy\n")
        )
        self.assertIn('if [[ "$PRIVATE_TRIAL_BUILD" == 1 ]]; then', items[build])
        self.assertIn(BUILD + " --features encrypted-upload-caller 2>&1 | tee", items[build])
        self.assertIn("else\n            " + BUILD + " 2>&1 | tee", items[build])
        self.assertIn("          private_playback=$(python3 -I ../../.github/scripts/staging_stream_config.py --api-vars)", items[generate])
        self.assertIn("--trial-db-bindings >> wrangler.toml", items[generate])
        self.assertLess(generate, cache)
        self.assertLess(cache, deploy)
        self.assertIn("needs.gate.outputs.private_trial_upload == '1'", items[cache])
        self.assertIn("if len(ids) != 4:", items[cache])
        self.assertIn('while IFS= read -r id; do\n            ./check_hyperdrive_cache.sh --config-id "$id"', items[cache])

    def test_private_upload_is_default_off_and_uses_distinct_staging_bindings(self):
        self.assertEqual(STREAM_CONFIG.api_vars({}), [])
        self.assertEqual(STREAM_CONFIG.trial_db_bindings({}), [])
        armed = {"PRIVATE_TRIAL_UPLOAD_STAGING_ENABLED": "true"}
        with self.assertRaisesRegex(ValueError, "staged playback"):
            STREAM_CONFIG.api_vars(armed)
        env = armed | {
            "PRIVATE_PLAYBACK_STAGING_ENABLED": "true",
            "PRIVATE_MUSIC_STAGING_JOURNEY": "true",
            "STREAM_STAGING_DEPLOY_ENABLED": "true",
            "STREAM_SERVICE_STAGING": "nntune-stream-staging",
            "PRIVATE_PLAYBACK_STAGING_ENVIRONMENT": "11111111-1111-4111-8111-111111111111",
            "PRIVATE_PLAYBACK_STAGING_LIBRARY": "22222222-2222-4222-8222-222222222222",
            "PRIVATE_PLAYBACK_STAGING_S3_CONFIGURATION": "33333333-3333-4333-8333-333333333333",
            "PRIVATE_PLAYBACK_STAGING_ROOT_VERSION": "44444444-4444-4444-8444-444444444444",
            "PRIVATE_PLAYBACK_STAGING_S3_BUCKET": STREAM_CONFIG.FIXTURE_BUCKET,
            "PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT": "https://objects.example.test",
            "PRIVATE_PLAYBACK_STAGING_S3_REGION": "test-region",
            "HYPERDRIVE_ID_MB": "d" * 32,
        }
        bindings = {
            f"PRIVATE_TRIAL_{role}_DB_STAGING_ID": str(index) * 32
            for index, role in enumerate(STREAM_CONFIG.TRIAL_DATABASES, 1)
        }
        variables = tomllib.loads("\n".join(STREAM_CONFIG.api_vars(env)))
        self.assertEqual(variables["PRIVATE_TRIAL_UPLOAD_ENABLED"], "true")
        self.assertEqual(variables["PRIVATE_TRIAL_S3_ENDPOINT"], "https://objects.example.test")
        config = tomllib.loads("\n".join(STREAM_CONFIG.trial_db_bindings(env | bindings)))
        self.assertEqual(set(config["env"]), {"staging"})
        self.assertEqual(len(config["env"]["staging"]["hyperdrive"]), 4)
        manifest = tomllib.loads((ROOT / ".github/scripts/binding_manifest.toml").read_text())
        ordinary = next(w for w in manifest["worker"] if w["name"] == "api")["binding"][0]
        for invalid in ("", "1" * 32, ordinary["prod"], ordinary["staging"]):
            with self.subTest(invalid=invalid), self.assertRaisesRegex(ValueError, "distinct scoped binding"):
                STREAM_CONFIG.trial_db_bindings(env | bindings | {"PRIVATE_TRIAL_AUTH_DB_STAGING_ID": invalid})

    def test_stream_config_is_internal_and_uses_the_dedicated_fixture_bucket(self):
        source = tomllib.loads((ROOT / "workers/stream/wrangler.toml").read_text())
        off = tomllib.loads(STREAM_CONFIG.render(source, {}))
        self.assertEqual(off["env"]["staging"]["vars"], {"ENVIRONMENT": "staging", "PRIVATE_PLAYBACK_ENABLED": "false"})
        env = {
            "PRIVATE_PLAYBACK_STAGING_ENABLED": "true", "PRIVATE_MUSIC_STAGING_JOURNEY": "true",
            "PRIVATE_PLAYBACK_STAGING_S3_CONFIGURATION": "11111111-1111-4111-8111-111111111111",
            "PRIVATE_PLAYBACK_STAGING_S3_BUCKET": STREAM_CONFIG.FIXTURE_BUCKET,
            "PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT": "https://objects.example.test",
            "PRIVATE_PLAYBACK_STAGING_S3_REGION": "test-region",
            "PRIVATE_PLAYBACK_STAGING_ROOT_VERSION": "22222222-2222-4222-8222-222222222222",
        }
        on = tomllib.loads(STREAM_CONFIG.render(source, env))
        self.assertEqual(set(on["env"]), {"staging"})
        stage = on["env"]["staging"]
        self.assertEqual(stage["services"], [{"binding": "API", "service": "nntune-api-staging"}])
        self.assertFalse(stage["workers_dev"] or stage["preview_urls"])
        self.assertNotIn("routes", stage)
        self.assertEqual(stage["vars"]["PRIVATE_PLAYBACK_S3_BUCKET"], STREAM_CONFIG.FIXTURE_BUCKET)
        for key, value in (
            ("PRIVATE_MUSIC_STAGING_JOURNEY", ""),
            ("PRIVATE_PLAYBACK_STAGING_S3_BUCKET", "nntunev2-audio-prod"),
            ("PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT", "https://name@objects.example.test"),
            ("PRIVATE_PLAYBACK_STAGING_S3_CONFIGURATION", "11111111-1111-5111-8111-111111111111"),
            ("PRIVATE_PLAYBACK_STAGING_ROOT_VERSION", "22222222-2222-4222-7222-222222222222"),
            ("PRIVATE_PLAYBACK_STAGING_ROOT_VERSION", ""),
        ):
            with self.subTest(key=key), self.assertRaises(ValueError):
                STREAM_CONFIG.render(source, env | {key: value})

    def test_upload_redaction_requires_boolean_true_in_metadata(self):
        boundary = b"------formdata-undici-test"

        def part(disposition, payload):
            return (boundary + b'\r\nContent-Disposition: form-data; name="' + disposition
                    + b'"\r\n\r\n' + payload + b'\r\n')

        enabled = b'{"observability":{"redact_query_string":true}}'
        metadata = part(b"metadata", enabled)
        # A matching string buried in the bundle is never metadata evidence.
        wasm = part(b"index.wasm", b"\x00asm\xff" + enabled)
        close = boundary + b"--\r\n"
        with tempfile.TemporaryDirectory() as tmp:
            body = Path(tmp) / "upload.multipart"
            with self.assertRaisesRegex(UPLOAD.Failure, "did not write the upload body"):
                UPLOAD.assert_upload_redaction(body, "api")
            body.write_bytes(wasm + metadata + close)
            UPLOAD.assert_upload_redaction(body, "api")

            for name, data, error in (
                ("no boundary", enabled, "multipart boundary"),
                ("truncated", metadata, "malformed Wrangler upload body"),
                ("no metadata", wasm + close, "exactly one"),
                ("duplicate", metadata + metadata + close, "exactly one"),
                ("invalid JSON", part(b"metadata", b"{") + close, "invalid .* JSON"),
                ("missing observability", part(b"metadata", b"{}") + close, "must set"),
                ("missing redaction", part(b"metadata", b'{"observability":{}}') + close, "must set"),
                ("disabled", part(b"metadata", enabled.replace(b"true", b"false")) + close, "must set"),
                ("not a boolean", part(b"metadata", enabled.replace(b"true", b"1")) + close, "must set"),
            ):
                with self.subTest(name=name):
                    body.write_bytes(data)
                    with self.assertRaisesRegex(UPLOAD.Failure, error):
                        UPLOAD.assert_upload_redaction(body, "api")

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
            renderer = source / UPLOAD.PRIVATE_CONFIG
            renderer.write_bytes((ROOT / UPLOAD.PRIVATE_CONFIG).read_bytes())
            with patch.object(UPLOAD, "ROOT", source):
                tree, wdir = UPLOAD.fixture(base, "api", "fixture")
            self.assertEqual(wdir, tree / "workers/api")
            self.assertEqual((tree / UPLOAD.MANIFEST).read_bytes(), manifest.read_bytes())
            self.assertEqual((tree / UPLOAD.PRIVATE_CONFIG).read_bytes(), renderer.read_bytes())
            for name, content in files.items():
                self.assertEqual((wdir / "build" / name).read_bytes(), content)

    def test_staging_prepares_and_builds_before_upload(self):
        text = (ROOT / ".github/workflows/deploy.yml").read_text()
        self.assertNotIn("cargo install", text)
        for worker in ("api", "mb", "ingest"):
            with self.subTest(worker=worker):
                section = job(text, f"staging-{worker}")
                items = steps(section)
                rust = next(i for i, s in enumerate(items) if "worker_rust_provenance.py rust" in s)
                install = next(i for i, s in enumerate(items) if "worker_tools.py install" in s)
                build = next(i for i, s in enumerate(items) if f"Build {worker} with canonical tools" in s)
                self.assertLess(rust, install)
                upload = next(i for i, s in enumerate(items) if "id: deploy\n" in s)
                first_credentials = next(i for i, s in enumerate(items) if "secrets.CLOUDFLARE_" in s)
                self.assertLess(install, build)
                self.assertLess(build, first_credentials)
                self.assertLess(build, upload)
                for step in (items[install], items[build]):
                    self.assertNotIn("secrets.", step)
                    if worker == "api" and step == items[build]:
                        self.assertEqual(step.split("        env:\n")[1].split("        run:")[0],
                                         "          PRIVATE_TRIAL_BUILD: ${{ needs.gate.outputs.private_trial_upload }}\n")
                    else:
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
        self.assertIn("worker_rust_provenance.py rust", section)
        self.assertLess(section.index("worker_rust_provenance.py rust"), section.index(BUILD))
        self.assertIn("worker_rust_provenance.py build-ledger", section)
        self.assertIn("run_ledger_tests()", section)
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

    def test_stream_profile_selects_tools_after_ordinary_builds_and_smoke(self):
        ci = job((ROOT / ".github/workflows/ci.yml").read_text(), "worker-release-builds")
        items = steps(ci)
        prepare = next(i for i, s in enumerate(items) if "worker_tools.py install stream" in s)
        smoke = next(i for i, s in enumerate(items) if "Exercise concurrent Rust span identity in real workerd" in s)
        build = items[prepare + 1]
        self.assertLess(smoke, prepare)
        self.assertIn("working-directory: workers/stream", build)
        self.assertIn(BUILD, build)
        self.assertIn('check-build "$RUNNER_TEMP/worker-stream.log" stream', build)
        self.assertIn("git diff --exit-code -- ../../Cargo.lock", build)
        self.assertIn("check_worker_upload.py --wrangler", items[prepare + 2])


if __name__ == "__main__":
    unittest.main()
