#!/usr/bin/env python3
"""Offline acceptance contract for retained web releases (Python stdlib only).

No cloud calls: tools execute with an empty PATH, no inherited credentials and
socket creation disabled. Run with `python3 ci/test-web-release-acceptance.py`.
The required Playwright unit bank also invokes these unittest classes.

Artifact CLI (`ci/web-release-artifact.py`):
  package --root DIR --config TOML --plan JSON --repository OWNER/REPO
          --sha SHA --run-id ID --attempt N --public-api-base-url STRING
          --output TAR_GZ
  verify --archive TAR_GZ --repository OWNER/REPO --sha SHA --run-id ID
         --attempt N --plan-id SHARED_PLAN_ID --destination EMPTY_DIR [--require-portable]
  render-config --verified-root DIR --source-config TOML
                --environment staging|production --release-version VERSION
                --output TOML
Success is a JSON object with `ok: true`. Refusal is nonzero, a JSON object
with `ok: false, code: <nonempty string>`, and no usable output. Archives have
one manifest.json and a web/ tree. The manifest has schema
nntune-web-release-v1, components [web], source {repository,sha,runId,attempt},
compatibility {date,flags}, build {publicApiBaseUrl: null|string}, the EXACT
shared-action plan object, and files [{path,size,sha256}] relative to web/.
The archive digest is independently bound by the shared action, not this
per-file manifest. Production verification/rendering refuses a baked API origin.

Evidence CLI (`ci/web-release-evidence.py`):
  certify --input JSON --manifest JSON --repository OWNER/REPO --sha SHA
          --run-id ID --attempt N --workflow PATH --artifact-id ID
          --artifact-name NAME --artifact-digest sha256:HEX --output JSON
          [--require-completed]
Input schema nntune-web-evidence-v1 wraps native GitHub `run` and `artifact`
objects and `jobPages` [{page,total_count,nextPage,jobs}]. Pages start at 1,
nextPage is the following page number or null, every page repeats total_count,
and the complete unique jobs must equal it. The caller must fetch every page.
The certificate has schema nntune-web-certificate-v1, components [web], source
{repository,sha,runId,attempt}, and evidence {runId,attempt,workflow,jobs,
artifactId,artifactName,artifactDigest} usable by the shared action. Required
job/step selectors below are trusted caller policy, not input assertions.
Only a canary successor can accept an in-progress source with ALL required
jobs/steps completed successfully; --require-completed is mandatory for stable.
"""

import copy
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import unittest
from unittest import mock


REPO_ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = "TogetherWeOwn/nntunev2"
SHA = "0123456789abcdef0123456789abcdef01234567"
RUN_ID = 4242
ATTEMPT = 2
WORKFLOW = ".github/workflows/deploy.yml"
ARTIFACT_ID = 7301
ARTIFACT_NAME = "certified-web-4242-2"
ARTIFACT_DIGEST = "sha256:" + "a" * 64
CANARY = "2026.1028.0-canary.3"
STABLE = "2026.1028.0"
# Exact required GitHub job names; these are NOT supplied by evidence JSON.
DEPLOY_JOB = "staging-web"
SYNTHETIC_JOB = "post-deploy-e2e / synthetic smoke (health)"
JOURNEY_JOB = "post-deploy-e2e / full journey (signup → byop → ingest → meter → limits)"
JOURNEY_STEP = "Run full journey"
ENFORCE_STEP = "Enforce journey evidence gate (all-skipped must fail)"
REQUIRED_JOBS = [DEPLOY_JOB, SYNTHETIC_JOB, JOURNEY_JOB]

# Opaque frozen plan fixture: no local CalVer allocation/reallocation code.
# Its id is the shared engine's canonical-JSON SHA-256 of the other fields.
PLAN = {
    "schema": "calver-plan-v1",
    "repository": REPOSITORY,
    "version": CANARY,
    "tag": "canary/v" + CANARY,
    "date": "2026-10-28",
    "channel": "canary",
    "namespace": "",
    "sha": SHA,
    "operation": "staging/4242/2",
    "policy": "staged-promotion",
    "latest": "auto",
    "repairDate": None,
    "sourceCanary": None,
    "source": {"runId": RUN_ID, "attempt": ATTEMPT, "workflow": WORKFLOW, "jobs": REQUIRED_JOBS},
    "evidence": None,
    "id": "843c6240d3c014c89df83c8351aa34d0c9fca6e703d600c3cc389f3ae761e372",
}

PAYLOAD = {
    "_worker.js/index.js": b'import app from "./chunks/app.js"; export default app;\n',
    "_worker.js/chunks/app.js": b'export default {fetch() { return new Response("saved bytes"); }};\n',
    "_worker.js/engine.wasm": b"\x00asm\x01\x00\x00\x00",
    "_worker.js/.opaque": b"hidden module companion\n",
    "assets/style.css": b"body{color:green}\n",
    ".well-known/probe.json": b'{"retained":true}\n',
    "robots.txt": b"User-agent: *\nDisallow: /private\n",
}
CONFIG = '''name = "release-fixture"
main = ".svelte-kit/cloudflare/_worker.js/index.js"
compatibility_date = "2025-03-01"
compatibility_flags = ["nodejs_compat"]
[assets]
directory = ".svelte-kit/cloudflare"
binding = "ASSETS"
[env.staging]
name = "release-fixture-staging"
[env.production]
name = "release-fixture-production"
'''
SOURCE = {"repository": REPOSITORY, "sha": SHA, "runId": RUN_ID, "attempt": ATTEMPT}

# Patch at the network boundary while running the REAL tool, not a fake tool.
OFFLINE_RUNNER = '''import runpy, socket, sys

def refuse(*args, **kwargs):
    raise RuntimeError("network forbidden in offline release acceptance")
socket.socket = refuse
socket.create_connection = refuse
sys.argv = sys.argv[1:]
from pathlib import Path
sys.path.insert(0, str(Path(sys.argv[0]).resolve().parent))
runpy.run_path(sys.argv[0], run_name="__main__")
'''


def digest(data):
    return hashlib.sha256(data).hexdigest()


def manifest_fixture():
    return {
        "schema": "nntune-web-release-v1",
        "components": ["web"],
        "source": dict(SOURCE),
        "compatibility": {"date": "2025-03-01", "flags": ["nodejs_compat"]},
        "build": {"publicApiBaseUrl": None},
        "plan": copy.deepcopy(PLAN),
        "files": [
            {"path": path, "size": len(data), "sha256": digest(data)}
            for path, data in sorted(PAYLOAD.items())
        ],
    }


def archive_fixture(path, manifest=None, members=None):
    """Independent tar writer: mutants never pass through the production packer."""
    if manifest is None:
        manifest = manifest_fixture()
    if members is None:
        members = [("web/" + name, data, "file") for name, data in PAYLOAD.items()]
    with tarfile.open(path, "w:gz", format=tarfile.PAX_FORMAT) as archive:
        entries = [("manifest.json", json.dumps(manifest).encode(), "file"), *members]
        for name, data, kind in entries:
            info = tarfile.TarInfo(name)
            if kind in ("symlink", "hardlink"):
                info.type = tarfile.SYMTYPE if kind == "symlink" else tarfile.LNKTYPE
                info.linkname = data
                archive.addfile(info)
            elif kind == "directory":
                info.type = tarfile.DIRTYPE
                archive.addfile(info)
            else:
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))


def interpreter_loader_env():
    """Keep shared-library Python runnable without inheriting the parent environment."""
    return {key: os.environ[key] for key in ("LD_LIBRARY_PATH",) if os.environ.get(key)}


class OfflineCase(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="web-release-acceptance-")
        self.addCleanup(self.temporary.cleanup)
        self.tmp = Path(self.temporary.name)
        self.config = self.tmp / "source.toml"
        self.config.write_text(CONFIG)
        self.plan = self.tmp / "plan.json"
        self.plan.write_text(json.dumps(PLAN))
        self.root = self.tmp / "build"
        self.root.mkdir()
        for name, data in PAYLOAD.items():
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        self.archive = self.tmp / "release.tar.gz"
        self.destination = self.tmp / "verified"
        self.rendered = self.tmp / "rendered.toml"

    def tool(self, name, *args):
        script = REPO_ROOT / "ci" / name
        self.assertTrue(script.is_file(), f"Missing offline release behavior: {script.name}")
        result = subprocess.run(
            [sys.executable, "-I", "-c", OFFLINE_RUNNER, str(script), *map(str, args)],
            cwd=self.tmp,
            env={"PATH": "", "PYTHONDONTWRITEBYTECODE": "1", "PYTHONIOENCODING": "utf-8", **interpreter_loader_env()},
            text=True, capture_output=True, timeout=15,
        )
        return result

    def success(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        response = json.loads(result.stdout)
        self.assertIs(response.get("ok"), True, response)
        return response

    def refusal(self, result):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        response = json.loads(result.stdout)
        self.assertIs(response.get("ok"), False, response)
        self.assertIsInstance(response.get("code"), str, response)
        self.assertTrue(response["code"].strip(), response)
        return response

    def package(self, origin=""):
        return self.tool(
            "web-release-artifact.py", "package", "--root", self.root,
            "--config", self.config, "--plan", self.plan, "--repository", REPOSITORY,
            "--sha", SHA, "--run-id", RUN_ID, "--attempt", ATTEMPT,
            "--public-api-base-url", origin, "--output", self.archive,
        )

    def verify(self, *extra):
        return self.tool(
            "web-release-artifact.py", "verify", "--archive", self.archive,
            "--repository", REPOSITORY, "--sha", SHA, "--run-id", RUN_ID,
            "--attempt", ATTEMPT, "--plan-id", PLAN["id"], "--destination", self.destination, *extra,
        )

    def render(self, environment="production", version=STABLE):
        return self.tool(
            "web-release-artifact.py", "render-config", "--verified-root", self.destination,
            "--source-config", self.config, "--environment", environment,
            "--release-version", version, "--output", self.rendered,
        )

    def saved_payload(self):
        return {
            p.relative_to(self.destination / "web").as_posix(): p.read_bytes()
            for p in (self.destination / "web").rglob("*") if p.is_file()
        }


class ArtifactTests(OfflineCase):
    def test_isolated_interpreter_starts_with_only_its_loader_path_preserved(self):
        loader_path = os.pathsep.join(filter(None, [
            os.environ.get("LD_LIBRARY_PATH"), str(self.tmp / "python-loader"),
        ]))
        with mock.patch.dict(os.environ, {
            "LD_LIBRARY_PATH": loader_path, "PATH": "parent-path-must-not-leak",
            "GITHUB_TOKEN": "parent-token-must-not-leak", "GH_TOKEN": "parent-token-must-not-leak",
            "PYTHONPATH": "parent-modules-must-not-leak", "PYTHONHOME": "parent-home-must-not-leak",
        }, clear=True), mock.patch("subprocess.run", wraps=subprocess.run) as spawn:
            result = self.tool("web-release-artifact.py", "--help")
        self.assertEqual(spawn.call_args.kwargs["env"], {
            "PATH": "", "PYTHONDONTWRITEBYTECODE": "1", "PYTHONIOENCODING": "utf-8",
            "LD_LIBRARY_PATH": loader_path,
        })
        self.assertEqual(spawn.call_args.args[0][:3], [sys.executable, "-I", "-c"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("usage:", result.stdout)

    def test_package_keeps_all_nested_static_hidden_and_wasm_bytes_and_the_exact_plan(self):
        self.success(self.package())
        with tarfile.open(self.archive) as archive:
            members = [m for m in archive.getmembers() if m.isfile()]
            self.assertEqual(len({m.name for m in members}), len(members))
            self.assertEqual({m.name for m in members}, {"manifest.json", *("web/" + p for p in PAYLOAD)})
            manifest = json.load(archive.extractfile("manifest.json"))
            self.assertEqual(manifest, manifest_fixture())
            for name, data in PAYLOAD.items():
                self.assertEqual(archive.extractfile("web/" + name).read(), data)
        self.success(self.verify("--require-portable"))
        self.assertEqual(self.saved_payload(), PAYLOAD)
        self.assertEqual(json.loads((self.destination / "manifest.json").read_text())["plan"], PLAN)

    def test_independently_written_complete_archive_is_accepted(self):
        archive_fixture(self.archive)
        self.success(self.verify("--require-portable"))
        self.assertEqual(self.saved_payload(), PAYLOAD)

    def test_canary_to_stable_rendering_changes_only_runtime_metadata_not_saved_bytes(self):
        archive_fixture(self.archive)
        self.success(self.verify("--require-portable"))
        original_manifest = (self.destination / "manifest.json").read_bytes()
        for environment, version in [("staging", CANARY), ("production", STABLE)]:
            with self.subTest(environment=environment):
                self.rendered = self.tmp / (environment + ".toml")
                self.success(self.render(environment, version))
                config = tomllib.loads(self.rendered.read_text())
                effective = {**config, **config.get("env", {}).get(environment, {})}
                self.assertEqual(effective["name"], "release-fixture-" + environment)
                self.assertEqual(effective["compatibility_date"], "2025-03-01")
                self.assertEqual(effective["compatibility_flags"], ["nodejs_compat"])
                self.assertEqual(effective["vars"], {"DEPLOYED_RELEASE_VERSION": version})
                self.assertEqual((self.rendered.parent / effective["main"]).resolve(), self.destination / "web/_worker.js/index.js")
                self.assertEqual((self.rendered.parent / effective["assets"]["directory"]).resolve(), self.destination / "web")
                self.assertEqual(effective["assets"]["binding"], "ASSETS")
                self.assertNotIn("build", config)
                self.assertNotIn("build", effective)
                self.assertEqual(self.saved_payload(), PAYLOAD)
                self.assertEqual((self.destination / "manifest.json").read_bytes(), original_manifest)

    def test_package_refuses_symlinks_and_hardlinks_instead_of_following_outside_bytes(self):
        outside = self.tmp / "outside.txt"
        outside.write_text("do not package")
        link = self.root / "unsafe"
        for kind in ["symlink", "hardlink"]:
            with self.subTest(kind=kind):
                if kind == "symlink":
                    link.symlink_to(outside)
                else:
                    os.link(outside, link)
                try:
                    self.refusal(self.package())
                    self.assertFalse(self.archive.exists())
                finally:
                    link.unlink()
                self.assertEqual(outside.read_text(), "do not package")

    def test_inventory_must_match_the_whole_tree_not_only_worker_entrypoint(self):
        for kind in ["missing-static", "extra-hidden", "tampered-wasm", "wrong-size", "duplicate-inventory", "inventory-traversal"]:
            with self.subTest(kind=kind):
                self.destination = self.tmp / ("refused-" + kind)
                manifest = manifest_fixture()
                members = [("web/" + p, data, "file") for p, data in PAYLOAD.items()]
                if kind == "missing-static":
                    members = [m for m in members if m[0] != "web/assets/style.css"]
                elif kind == "extra-hidden":
                    members.append(("web/.unlisted", b"extra", "file"))
                elif kind == "tampered-wasm":
                    members = [(p, b"\x00asm\x01\x00\x00\x01" if p.endswith(".wasm") else data, t) for p, data, t in members]
                elif kind == "wrong-size":
                    manifest["files"][0]["size"] += 1
                elif kind == "duplicate-inventory":
                    manifest["files"].append(copy.deepcopy(manifest["files"][0]))
                else:
                    manifest["files"][0]["path"] = "../escape.txt"
                archive_fixture(self.archive, manifest, members)
                self.refusal(self.verify())
                self.assertFalse((self.destination / "manifest.json").exists(), "refused archive left a usable manifest")

    def test_ambiguous_or_escaping_archive_members_refuse_without_writing_outside_destination(self):
        sentinel = self.tmp / "escape.txt"
        sentinel.write_bytes(b"unchanged")
        mutants = [
            ("../escape.txt", b"overwritten", "file"),
            (str(sentinel), b"overwritten", "file"),
            ("web/../../escape.txt", b"overwritten", "file"),
            ("web/../manifest.json", b"{}", "file"),
            ("web\\assets\\style.css", b"alias", "file"),
            ("web//assets/style.css", b"alias", "file"),
            ("web/./assets/style.css", b"alias", "file"),
            ("web/assets/style.css", b"duplicate", "file"),
            ("manifest.json", b"{}", "file"),
            ("web/link", "../escape.txt", "symlink"),
            ("web/link", "../escape.txt", "hardlink"),
            ("unlisted-root-file", b"extra", "file"),
        ]
        for index, member in enumerate(mutants):
            with self.subTest(member=member[0], type=member[2]):
                self.destination = self.tmp / ("unsafe-" + str(index))
                members = [("web/" + p, data, "file") for p, data in PAYLOAD.items()]
                archive_fixture(self.archive, members=[*members, member])
                self.refusal(self.verify())
                self.assertEqual(sentinel.read_bytes(), b"unchanged")
                self.assertFalse((self.destination / "manifest.json").exists())

    def test_wrong_source_plan_or_component_cannot_be_promoted(self):
        mutants = [
            ("repository", "other/repository"), ("sha", "f" * 40),
            ("runId", RUN_ID + 1), ("attempt", ATTEMPT + 1),
            ("components", ["web", "api"]), ("components", []),
            ("plan-sha", "f" * 40), ("plan-attempt", ATTEMPT + 1),
            ("plan-version", "2026.1028.1-canary.0"),
        ]
        for key, value in mutants:
            with self.subTest(key=key):
                self.destination = self.tmp / ("identity-" + key + "-" + str(len(str(value))))
                manifest = manifest_fixture()
                if key == "components":
                    manifest[key] = value
                elif key == "plan-sha":
                    manifest["plan"]["sha"] = value
                elif key == "plan-attempt":
                    manifest["plan"]["source"]["attempt"] = value
                elif key == "plan-version":
                    manifest["plan"]["version"] = value
                else:
                    manifest["source"][key] = value
                archive_fixture(self.archive, manifest)
                self.refusal(self.verify())
                self.assertFalse((self.destination / "manifest.json").exists())

    def test_a_different_self_consistent_plan_cannot_replace_the_frozen_expected_plan(self):
        manifest = manifest_fixture()
        replacement = manifest["plan"]
        replacement["operation"] = "staging/4242/2/reallocated"
        del replacement["id"]
        replacement["id"] = digest(json.dumps(
            replacement, sort_keys=True, separators=(",", ":"), ensure_ascii=False,
        ).encode())
        archive_fixture(self.archive, manifest)
        self.refusal(self.verify())
        self.assertFalse((self.destination / "manifest.json").exists())

    def test_truncated_or_nonarchive_bytes_refuse_without_a_usable_extraction(self):
        for index, content in enumerate([b"not an archive", b"\x1f\x8b\x08\x00truncated"]):
            with self.subTest(case=index):
                self.destination = self.tmp / ("truncated-" + str(index))
                self.archive.write_bytes(content)
                self.refusal(self.verify())
                self.assertFalse((self.destination / "manifest.json").exists())

    def test_package_refuses_a_plan_for_another_source_instead_of_restamping_it(self):
        plan = copy.deepcopy(PLAN)
        plan["source"]["attempt"] = ATTEMPT - 1
        self.plan.write_text(json.dumps(plan))
        self.refusal(self.package())
        self.assertFalse(self.archive.exists())

    def test_destination_must_be_empty_and_not_a_link(self):
        archive_fixture(self.archive)
        self.destination.mkdir()
        sentinel = self.destination / "already-there"
        sentinel.write_text("keep")
        self.refusal(self.verify())
        self.assertEqual(sentinel.read_text(), "keep")
        linked = self.tmp / "linked-destination"
        linked.symlink_to(self.destination, target_is_directory=True)
        self.destination = linked
        self.refusal(self.verify())
        self.assertEqual(sentinel.read_text(), "keep")

    def test_a_staging_only_baked_api_origin_is_retained_but_refuses_promotion(self):
        self.success(self.package("https://staging-api.example.test"))
        self.refusal(self.verify("--require-portable"))
        self.assertFalse((self.destination / "manifest.json").exists())
        self.destination = self.tmp / "staging-only"
        self.success(self.verify())
        self.refusal(self.render())
        self.assertFalse(self.rendered.exists())

    def test_render_revalidates_saved_bytes_and_compatibility_instead_of_silently_rebuilding(self):
        archive_fixture(self.archive)
        self.success(self.verify("--require-portable"))
        worker = self.destination / "web/_worker.js/index.js"
        worker.write_bytes(b"source substitution")
        self.refusal(self.render())
        self.assertFalse(self.rendered.exists())
        worker.write_bytes(PAYLOAD["_worker.js/index.js"])
        self.config.write_text(CONFIG.replace("2025-03-01", "2026-01-01"))
        self.refusal(self.render())
        self.assertFalse(self.rendered.exists())

    def test_render_refuses_build_hooks_and_production_canary_or_noncanonical_release_values(self):
        archive_fixture(self.archive)
        self.success(self.verify("--require-portable"))
        for version in [CANARY, "v" + STABLE, "2026.0105.0", "2026.229.0", "2026.1028.00", STABLE + "\nINJECT=1"]:
            with self.subTest(version=version):
                self.refusal(self.render(version=version))
                self.assertFalse(self.rendered.exists())
        self.config.write_text(CONFIG + '\n[build]\ncommand = "npm run build"\n')
        self.refusal(self.render())
        self.assertFalse(self.rendered.exists())


def step_fixture(name, number):
    return {
        "name": name, "number": number, "status": "completed", "conclusion": "success",
        "started_at": "2026-10-28T10:01:00Z", "completed_at": "2026-10-28T10:02:00Z",
    }


def job_fixture(name, identifier, steps):
    return {
        "id": identifier, "run_id": RUN_ID, "run_attempt": ATTEMPT, "head_sha": SHA,
        "name": name, "status": "completed", "conclusion": "success", "steps": steps,
        "started_at": "2026-10-28T10:00:00Z", "completed_at": "2026-10-28T10:15:00Z",
    }


def evidence_fixture():
    repository = {"id": 99, "full_name": REPOSITORY}
    jobs = [
        job_fixture(DEPLOY_JOB, 8001, [step_fixture("Deploy to staging", 1)]),
        job_fixture(SYNTHETIC_JOB, 8002, [step_fixture("Run synthetic smoke", 1)]),
        job_fixture(JOURNEY_JOB, 8003, [step_fixture(JOURNEY_STEP, 1), step_fixture(ENFORCE_STEP, 2)]),
    ]
    return {
        "schema": "nntune-web-evidence-v1",
        "run": {
            "id": RUN_ID, "run_attempt": ATTEMPT, "head_sha": SHA,
            "head_branch": "main", "event": "push", "path": WORKFLOW,
            "status": "completed", "conclusion": "success",
            "repository": copy.deepcopy(repository), "head_repository": copy.deepcopy(repository),
            "run_started_at": "2026-10-28T10:00:00Z", "updated_at": "2026-10-28T10:20:00Z",
        },
        "jobPages": [{"page": 1, "total_count": len(jobs), "nextPage": None, "jobs": jobs}],
        "artifact": {
            "id": ARTIFACT_ID, "name": ARTIFACT_NAME, "digest": ARTIFACT_DIGEST,
            "expired": False, "size_in_bytes": 4096,
            "created_at": "2026-10-28T10:10:00Z", "expires_at": "2099-01-01T00:00:00Z",
            "workflow_run": {
                "id": RUN_ID, "repository_id": 99, "head_repository_id": 99,
                "head_sha": SHA, "head_branch": "main",
            },
        },
    }


class EvidenceTests(OfflineCase):
    def certify(self, fixture, manifest=None, *extra):
        input_file = self.tmp / "evidence.json"
        input_file.write_text(json.dumps(fixture))
        manifest_file = self.tmp / "manifest.json"
        manifest_file.write_text(json.dumps(manifest if manifest is not None else manifest_fixture()))
        self.certificate = self.tmp / "certificate.json"
        return self.tool(
            "web-release-evidence.py", "certify", "--input", input_file,
            "--manifest", manifest_file, "--repository", REPOSITORY, "--sha", SHA,
            "--run-id", RUN_ID, "--attempt", ATTEMPT, "--workflow", WORKFLOW,
            "--artifact-id", ARTIFACT_ID, "--artifact-name", ARTIFACT_NAME,
            "--artifact-digest", ARTIFACT_DIGEST, "--output", self.certificate, *extra,
        )

    def assert_refused(self, fixture, manifest=None, *extra):
        self.refusal(self.certify(fixture, manifest, *extra))
        self.assertFalse(self.certificate.exists(), "refused evidence minted a certificate")

    def test_real_executed_steps_yield_a_web_only_exact_attempt_artifact_selector(self):
        self.success(self.certify(evidence_fixture(), None, "--require-completed"))
        certificate = json.loads(self.certificate.read_text())
        self.assertEqual(certificate["schema"], "nntune-web-certificate-v1")
        self.assertEqual(certificate["components"], ["web"])
        self.assertEqual(certificate["source"], SOURCE)
        self.assertEqual(certificate["evidence"], {
            "runId": RUN_ID, "attempt": ATTEMPT, "workflow": WORKFLOW, "jobs": REQUIRED_JOBS,
            "artifactId": ARTIFACT_ID, "artifactName": ARTIFACT_NAME, "artifactDigest": ARTIFACT_DIGEST,
        })

    def test_complete_multiple_job_pages_are_accepted_but_truncated_pages_never_are(self):
        fixture = evidence_fixture()
        jobs = fixture["jobPages"][0]["jobs"]
        fixture["jobPages"] = [
            {"page": 1, "total_count": 3, "nextPage": 2, "jobs": jobs[:1]},
            {"page": 2, "total_count": 3, "nextPage": None, "jobs": jobs[1:]},
        ]
        self.success(self.certify(fixture))
        self.certificate.unlink()
        for mutant in [
            [fixture["jobPages"][0]],
            [{**fixture["jobPages"][0], "nextPage": None}],
            [{**fixture["jobPages"][0], "nextPage": 3}, fixture["jobPages"][1]],
            [fixture["jobPages"][0], {**fixture["jobPages"][1], "nextPage": 3}],
            [fixture["jobPages"][0], {**fixture["jobPages"][1], "total_count": 4}],
        ]:
            with self.subTest(pages=mutant):
                self.assert_refused({**fixture, "jobPages": mutant})

    def test_in_progress_canary_successor_requires_finished_jobs_but_stable_requires_completed_run(self):
        fixture = evidence_fixture()
        fixture["run"].update(status="in_progress", conclusion=None)
        self.success(self.certify(fixture))
        self.certificate.unlink()
        self.assert_refused(fixture, None, "--require-completed")
        fixture["jobPages"][0]["jobs"][2].update(status="in_progress", conclusion=None)
        self.assert_refused(fixture)

    def test_unconfigured_green_jobs_with_skipped_acceptance_steps_cannot_certify(self):
        for step_name in [JOURNEY_STEP, ENFORCE_STEP, "Run synthetic smoke", "Deploy to staging"]:
            for state in ["skipped", "failure", "cancelled", None]:
                with self.subTest(step=step_name, conclusion=state):
                    fixture = evidence_fixture()
                    for job in fixture["jobPages"][0]["jobs"]:
                        for step in job["steps"]:
                            if step["name"] == step_name:
                                step["conclusion"] = state
                    self.assert_refused(fixture)
        fixture = evidence_fixture()
        for job in fixture["jobPages"][0]["jobs"]:
            for step in job["steps"]:
                step["conclusion"] = "skipped"
        self.assert_refused(fixture)

    def test_missing_duplicate_or_running_required_jobs_and_steps_refuse(self):
        for kind in ["missing-job", "duplicate-job", "duplicate-id", "missing-step", "duplicate-step", "running-step", "failed-job", "skipped-job"]:
            with self.subTest(kind=kind):
                fixture = evidence_fixture()
                page = fixture["jobPages"][0]
                journey = page["jobs"][2]
                if kind == "missing-job":
                    page["jobs"].pop(2)
                elif kind == "duplicate-job":
                    page["jobs"].append({**copy.deepcopy(journey), "id": 9001})
                elif kind == "duplicate-id":
                    page["jobs"][1]["id"] = page["jobs"][0]["id"]
                elif kind == "missing-step":
                    journey["steps"].pop()
                elif kind == "duplicate-step":
                    journey["steps"].append({**journey["steps"][1], "number": 3})
                elif kind == "running-step":
                    journey["steps"][0]["status"] = "in_progress"
                else:
                    journey["conclusion"] = "failure" if kind == "failed-job" else "skipped"
                page["total_count"] = len(page["jobs"])
                self.assert_refused(fixture)

    def test_manual_scheduled_fault_failed_foreign_wrong_sha_run_or_attempt_never_substitute(self):
        mutants = [
            ("event", "workflow_dispatch"), ("event", "schedule"), ("event", "pull_request"),
            ("event", "workflow_run"), ("event", "repository_dispatch"),
            ("head_branch", "feature"), ("head_sha", "f" * 40),
            ("id", RUN_ID + 1), ("run_attempt", ATTEMPT + 1),
            ("path", ".github/workflows/e2e-staging.yml"),
            ("status", "queued"), ("conclusion", "failure"), ("conclusion", "cancelled"),
            ("repository", {"id": 100, "full_name": "other/repository"}),
            ("head_repository", {"id": 101, "full_name": "fork/nntunev2"}),
        ]
        for key, value in mutants:
            with self.subTest(key=key, value=value):
                fixture = evidence_fixture()
                fixture["run"][key] = value
                self.assert_refused(fixture)
        for key, value in [("run_id", RUN_ID + 1), ("run_attempt", ATTEMPT - 1), ("head_sha", "f" * 40)]:
            fixture = evidence_fixture()
            fixture["jobPages"][0]["jobs"][2][key] = value
            self.assert_refused(fixture)
        # A fault/recovery run remains ineligible even if a caller falsifies
        # its job-level conclusion: the real journey step did not succeed.
        fixture = evidence_fixture()
        fixture["jobPages"][0]["jobs"][2]["steps"][0]["conclusion"] = "failure"
        self.assert_refused(fixture)

    def test_expired_missing_mismatched_or_other_attempt_artifacts_refuse(self):
        mutants = [
            ("id", ARTIFACT_ID + 1), ("name", ARTIFACT_NAME + "-rebuilt"),
            ("digest", "sha256:" + "b" * 64), ("digest", None),
            ("expired", True), ("size_in_bytes", 0),
            ("created_at", "2026-10-28T09:59:59Z"),
            ("expires_at", "2020-01-01T00:00:00Z"),
            ("workflow_run", {"id": RUN_ID + 1, "head_sha": SHA}),
        ]
        for key, value in mutants:
            with self.subTest(key=key):
                fixture = evidence_fixture()
                fixture["artifact"][key] = value
                self.assert_refused(fixture)
        for key, value in [("head_sha", "f" * 40), ("head_branch", "feature"), ("repository_id", 100), ("head_repository_id", 100)]:
            fixture = evidence_fixture()
            fixture["artifact"]["workflow_run"][key] = value
            self.assert_refused(fixture)
        fixture = evidence_fixture()
        del fixture["artifact"]
        self.assert_refused(fixture)

    def test_every_required_job_and_step_must_be_unique_and_completed_not_just_the_journey(self):
        for index, name in enumerate(REQUIRED_JOBS):
            for kind in ["missing", "duplicate", "failed", "skipped", "running"]:
                with self.subTest(job=name, kind=kind):
                    fixture = evidence_fixture()
                    page = fixture["jobPages"][0]
                    target = page["jobs"][index]
                    if kind == "missing":
                        page["jobs"].pop(index)
                    elif kind == "duplicate":
                        page["jobs"].append({**copy.deepcopy(target), "id": 9001})
                    elif kind == "running":
                        target["status"] = "in_progress"
                    else:
                        target["conclusion"] = "failure" if kind == "failed" else "skipped"
                    page["total_count"] = len(page["jobs"])
                    self.assert_refused(fixture)
        for index, step_index in [(0, 0), (1, 0), (2, 0), (2, 1)]:
            for kind in ["missing", "duplicate", "running"]:
                with self.subTest(job_index=index, step_index=step_index, kind=kind):
                    fixture = evidence_fixture()
                    steps = fixture["jobPages"][0]["jobs"][index]["steps"]
                    if kind == "missing":
                        steps.pop(step_index)
                    elif kind == "duplicate":
                        steps.append({**steps[step_index], "number": 99})
                    else:
                        steps[step_index]["status"] = "in_progress"
                    self.assert_refused(fixture)

    def test_manifest_source_and_frozen_plan_cannot_disagree_with_github_evidence(self):
        for key, value in [("repository", "other/repository"), ("sha", "f" * 40), ("runId", RUN_ID + 1), ("attempt", ATTEMPT - 1)]:
            with self.subTest(key=key):
                manifest = manifest_fixture()
                manifest["source"][key] = value
                self.assert_refused(evidence_fixture(), manifest)
        manifest = manifest_fixture()
        manifest["plan"]["source"]["jobs"] = ["staging-api"]
        self.assert_refused(evidence_fixture(), manifest)
        fixture = evidence_fixture()
        fixture["schema"] = "unknown-evidence-schema"
        self.assert_refused(fixture)

    def test_an_api_lane_success_cannot_replace_web_steps_or_expand_the_certificate(self):
        fixture = evidence_fixture()
        page = fixture["jobPages"][0]
        page["jobs"].append(job_fixture("staging-api", 8004, [step_fixture("Deploy API to staging", 1)]))
        page["total_count"] = 4
        self.success(self.certify(fixture))
        self.assertEqual(json.loads(self.certificate.read_text())["components"], ["web"])
        self.certificate.unlink()
        for components in [["api"], ["web", "api"], []]:
            manifest = manifest_fixture()
            manifest["components"] = components
            self.assert_refused(fixture, manifest)
        page["jobs"] = [job for job in page["jobs"] if job["name"] != JOURNEY_JOB]
        page["total_count"] = 3
        self.assert_refused(fixture)


if __name__ == "__main__":
    unittest.main()
