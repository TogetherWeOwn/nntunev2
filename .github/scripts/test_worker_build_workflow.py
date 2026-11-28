"""Regression tests for canonical Worker build and upload sequencing."""

import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
from textwrap import dedent
import tomllib
import unittest
from unittest.mock import Mock, patch
from uuid import uuid4

SPEC = importlib.util.spec_from_file_location("check_worker_upload", Path(__file__).with_name("check_worker_upload.py"))
UPLOAD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(UPLOAD)
CONFIG_SPEC = importlib.util.spec_from_file_location(
    "staging_stream_config", Path(__file__).with_name("staging_stream_config.py"),
)
STREAM_CONFIG = importlib.util.module_from_spec(CONFIG_SPEC)
CONFIG_SPEC.loader.exec_module(STREAM_CONFIG)
WORKFLOW_SPEC = importlib.util.spec_from_file_location(
    "workflow_structure", Path(__file__).with_name("test_ci_pnpm_retry.py"),
)
WORKFLOW = importlib.util.module_from_spec(WORKFLOW_SPEC)
WORKFLOW_SPEC.loader.exec_module(WORKFLOW)

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
    def test_worker_tool_installs_use_rust_from_an_earlier_step(self):
        rust = re.compile(r"\bworker_rust_provenance\.py[ \t]+rust\b")
        install = re.compile(r"\bworker_tools\.py[ \t]+install\b(?![ \t]+repair\b)")
        run = re.compile(r"^ {6}(?:- |  )run:.*(?:\n(?: {10}.*|[ \t]*(?=\n|$)))*", re.M)
        installations = 0
        for path in sorted((ROOT / ".github/workflows").glob("*.yml")):
            for name, section in WORKFLOW.split_jobs(path.read_text()).items():
                rust_idx = None
                for index, step in enumerate(WORKFLOW.split_steps(section)):
                    match = run.search(step)
                    if match is None:
                        continue
                    script = re.sub(r"(?m)^[ \t]*#.*$", "", match.group())
                    if rust.search(script):
                        rust_idx = index
                    if install.search(script):
                        installations += 1
                        with self.subTest(job=f"{path.name}:{name}", step=index + 1):
                            self.assertTrue(
                                rust_idx is not None and rust_idx < index,
                                "Worker tool install requires Rust setup in an earlier, separate step",
                            )
        self.assertGreater(installations, 0, "no canonical Worker tool installs found")

    def test_api_wrangler_example_requires_query_redaction(self):
        example = (ROOT / "workers/api/wrangler.toml.example").read_text()
        _, _, block = example.partition("# [env.staging.observability]\n")
        block = block.split("\n\n", 1)[0]
        observability = tomllib.loads("\n".join(line.removeprefix("# ") for line in block.splitlines()))
        self.assertIs(observability.get("redact_query_string"), True)

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

        url_error = "synthetic invocation URL is missing or differs from the redacted URL"
        for case, raw, diagnostic in (
            ("missing event", b"", "missing synthetic invocation record"),
            ("empty event", b"{}", "missing or malformed synthetic invocation record"),
            ("wrong method", json.dumps(event | {"method": "POST"}).encode(),
             "expected a synthetic GET invocation record"),
            ("wrong event kind", json.dumps(event | {"kind": "log"}).encode(),
             "expected a synthetic GET invocation record"),
            ("wrong Worker", json.dumps(event | {"worker": "synthetic-web-worker"}).encode(),
             "unexpected synthetic invocation Worker"),
            ("stale marker", json.dumps(event | {"probe_id": uuid4().hex}).encode(),
             "synthetic invocation does not match this probe"),
            ("missing URL", json.dumps(event | {"url": None}).encode(), url_error),
            ("retained query", json.dumps(event | {"url": probe_url}).encode(), url_error),
            ("query delimiter", json.dumps(event | {"url": clean_url + "?"}).encode(), url_error),
            *((
                f"retained {key}",
                json.dumps(event | {"url": f"{clean_url}?{key}=synthetic-canary"}).encode(),
                url_error,
            ) for key in ("p", "t", "s", "u", "apiKey", "password", "token")),
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
        self.assertIn('expected = 1 if os.environ.get("PRIVATE_TRIAL_IDENTITY_MODE") == "upload" else 4', items[cache])
        self.assertIn('while IFS= read -r id; do\n            ./check_hyperdrive_cache.sh --config-id "$id"', items[cache])

    def test_private_upload_requires_explicitly_disabled_cache_on_each_connection(self):
        workflow = (ROOT / UPLOAD.WORKFLOW).read_text()
        step = UPLOAD.step_text(UPLOAD.job_text(workflow, "staging-api"), "Require uncached private trial connections")
        script = UPLOAD.run_script(step)
        config_ids = {
            role.lower(): f"{index:032x}"
            for index, role in enumerate(("UPLOAD", "AUTH", "PUBLISH", "ENROLL"), 1)
        }
        bindings = {f"PRIVATE_TRIAL_{role.upper()}_DB_STAGING_ID": value for role, value in config_ids.items()}
        ordinary_id = "c" * 32
        config = ['[[env.staging.hyperdrive]]', 'binding = "HYPERDRIVE"', f'id = "{ordinary_id}"']
        config.extend(STREAM_CONFIG.trial_db_bindings({"PRIVATE_TRIAL_UPLOAD_STAGING_ENABLED": "true", **bindings}))
        provider = "https://api.cloudflare.com/client/v4/accounts/" + "a" * 32 + "/hyperdrive/configs/"
        # Cloudflare's GET wraps the config and its caching object in result.
        responses = {
            provider + config_id: {
                "success": True, "errors": [], "messages": [],
                "result": {
                    "id": config_id, "name": f"trial-{role}",
                    "origin": {
                        "host": "trial-db.example.test", "database": "synthetic_trial",
                        "scheme": "postgresql", "user": f"synthetic_{role}", "access_client_id": "fixture-client",
                    },
                    "mtls": {"sslmode": "verify-full", "ca_certificate_id": "b" * 32},
                    "caching": {"disabled": True},
                },
            }
            for role, config_id in config_ids.items()
        }

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            (work / "wrangler.toml").write_text("\n".join(config))
            (work / "check_hyperdrive_cache.sh").symlink_to(ROOT / "workers/api/check_hyperdrive_cache.sh")
            # Give the workflow shell only the tools this fixture needs. Curl
            # serves local fixtures and refuses URLs outside the four expected GETs.
            for command in ("bash", "python3", "mktemp", "rm"):
                path = shutil.which(command)
                self.assertIsNotNone(path, command)
                (work / command).symlink_to(path)
            curl = work / "curl"
            curl.write_text("#!" + sys.executable + " -I\n" + dedent("""\
                import json
                from pathlib import Path
                import sys

                args = sys.argv[1:]
                url = args[-1]
                response = json.loads(Path("provider.json").read_text())[url]
                with Path("requests").open("a") as requests:
                    requests.write(url + "\\n")
                Path(args[args.index("-o") + 1]).write_text(json.dumps(response))
                print(200)
                """))
            curl.chmod(0o700)
            requests = work / "requests"

            def run_cache_step():
                (work / "provider.json").write_text(json.dumps(responses))
                requests.write_text("")
                return subprocess.run([str(work / "bash"), "-c", script], cwd=work, capture_output=True,
                                      text=True, timeout=10, env={
                                          "PATH": tmp, "TMPDIR": tmp,
                                          "CLOUDFLARE_ACCOUNT_ID": "a" * 32,
                                          "CLOUDFLARE_API_TOKEN": "fixture-token",
                                      })

            passed = run_cache_step()
            self.assertEqual(passed.returncode, 0, passed.stderr)
            self.assertCountEqual(requests.read_text().splitlines(), responses.keys())

            for response in responses.values():
                result = response["result"]
                for caching in (None, {}, {"disabled": False}):
                    with self.subTest(config=result["id"], caching=caching):
                        if caching is None:
                            del result["caching"]
                        else:
                            result["caching"] = caching
                        refused = run_cache_step()
                        self.assertEqual(refused.returncode, 1, refused.stderr)
                        self.assertIn(f'config {result["id"]} must have query caching disabled', refused.stderr)
                result["caching"] = {"disabled": True}

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

    def test_lost_trial_connection_create_prepares_original_binding(self):
        # Offline C1 runbook fixture. These are normalized, synthetic records
        # from the attempt, provider lookup and protected SQL/transport pins.
        request_ref = "synthetic-enroll-request"
        requested = {
            "account": "synthetic-trial-account",
            "binding": "PRIVATE_TRIAL_ENROLL_DB",
            "database": ("nntunev2_staging", 40200),
            "role": ("fixture_enroll", 40204),
            "origin": "trial.example.invalid",
            "access_client_ref": "synthetic-serving-client",
            "origin_ca_ref": "synthetic-trial-ca",
            "sslmode": "verify-full",
            "caching_disabled": True,
        }
        config_id = "4" * 32
        configs, protected_results = [], {}

        def lose_create_response(ref, scope):
            configs.append(scope | {"id": config_id})
            protected_results[scope["account"], ref] = config_id
            raise TimeoutError("synthetic response lost after create")

        create = Mock(side_effect=lose_create_response)

        def plan_handoff(results, readbacks, *, sent):
            if not sent:
                return {"next_action": "create", "connection": None}
            bound_id = results.get((requested["account"], request_ref))
            expected = requested | {"id": bound_id}
            matches = [row for row in readbacks if row == expected] if bound_id else []
            if len(matches) != 1:
                return {"next_action": "reconcile", "connection": None}
            return {"next_action": "prepare", "connection": matches[0]}

        def execute_create_step(plan):
            if plan["next_action"] == "create":
                create(request_ref, requested)

        with self.assertRaises(TimeoutError):
            execute_create_step(plan_handoff({}, [], sent=False))

        original = requested | {"id": config_id}
        unrelated = original | {"id": "9" * 32}
        handoff = plan_handoff(protected_results, [unrelated, *configs], sent=True)
        self.assertEqual(handoff, {"next_action": "prepare", "connection": original})
        execute_create_step(handoff)
        for case, results, readbacks in (
            ("missing", protected_results, [unrelated]),
            ("duplicate", protected_results, configs * 2),
            ("unbound", {}, configs),
            ("wrong request", {(requested["account"], "synthetic-other-request"): config_id}, configs),
            ("wrong account", protected_results, [original | {"account": "other-account"}]),
            ("wrong role", protected_results, [original | {"role": ("fixture_enroll", 40205)}]),
        ):
            with self.subTest(case=case):
                self.assertEqual(plan_handoff(results, readbacks, sent=True),
                                 {"next_action": "reconcile", "connection": None})

        connection = handoff["connection"]
        already_verified = {"UPLOAD": "1" * 32, "AUTH": "2" * 32, "PUBLISH": "3" * 32}
        inputs = {f"PRIVATE_TRIAL_{role}_DB_STAGING_ID": value for role, value in already_verified.items()}
        inputs[f"{connection['binding']}_STAGING_ID"] = connection["id"]
        rendered = STREAM_CONFIG.trial_db_bindings(inputs | {"PRIVATE_TRIAL_UPLOAD_STAGING_ENABLED": "true"})
        bindings = tomllib.loads("\n".join(rendered))["env"]["staging"]["hyperdrive"]
        self.assertEqual(bindings, [
            {"binding": f"PRIVATE_TRIAL_{role}_DB", "id": value}
            for role, value in (already_verified | {"ENROLL": config_id}).items()
        ])
        create.assert_called_once_with(request_ref, requested)

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
                ("duplicate JSON field", part(b"metadata",
                   b'{"observability":{"redact_query_string":false,"redact_query_string":true}}') + close,
                 "duplicate fields"),
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
                                         """          PRIVATE_TRIAL_BUILD: ${{ contains(fromJSON('["upload","all"]'), inputs.private_trial_identity_mode) && '1' || needs.gate.outputs.private_trial_upload }}\n""")
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

    def test_candidate_upload_uses_verified_preparation_outputs(self):
        ci = job((ROOT / ".github/workflows/ci.yml").read_text(), "worker-release-builds")
        items = steps(ci)
        dry_run = next(i for i, s in enumerate(items) if "check_worker_upload.py --wrangler" in s)
        prepare, upload = items[dry_run + 1:dry_run + 3]
        self.assertIn("id: api-candidate\n", prepare)
        self.assertIn("run: python3 -B ci/staging_api_candidate.py\n", prepare)
        self.assertIn("github.event_name == 'push'", prepare)
        self.assertIn("github.ref == 'refs/heads/main' && github.ref_protected", prepare)
        self.assertIn("github.run_attempt == 1", prepare)
        self.assertIn("if: steps.api-candidate.outcome == 'success'\n", upload)
        self.assertIn("uses: actions/upload-artifact@", upload)
        self.assertIn("name: ${{ steps.api-candidate.outputs.name }}\n", upload)
        self.assertIn("path: ${{ steps.api-candidate.outputs.path }}\n", upload)


if __name__ == "__main__":
    unittest.main()
