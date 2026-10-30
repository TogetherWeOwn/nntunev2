#!/usr/bin/env python3
"""Independent producer-closure, malformed-input and GitHub wire regressions.

Run with Python stdlib only. Imported by the existing required offline bank;
no cloud, real HTTP, production dispatch or alternative allocator is involved.
Fixtures derive from the public CLI/GitHub contracts, not product helpers.

Producer `package` additionally accepts --bundle-root DIR. Every emitted file
is retained below web/_worker.js/, with only emitted _worker.js renamed to
index.js. Canonical destination collisions refuse rather than overwrite.

Evidence read CLIs:
  fetch --repository OWNER/REPO --run-id ID --attempt N --artifact-id ID
        --artifact-name NAME --artifact-digest sha256:ZIP_HEX
        --archive-output TAR_GZ --output NATIVE_EVIDENCE_JSON
  resolve --repository OWNER/REPO --canary EXACT_TAG --sha SHA --output JSON
`fetch` returns the root ZIP member web-release.tar.gz, never rebuilt bytes;
its metadata is the original nntune-web-evidence-v1 envelope, not a certificate.
`resolve` returns {plan, evidence} from exact annotated-tag provenance. Neither
mode allocates versions or performs non-GET requests. The fake HTTP handler
feeds standard native responses while real urllib redirect/error policy runs.
"""

import base64
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import tarfile
import unittest
from unittest import mock
import urllib.parse
import zipfile


fixtures = runpy.run_path(str(Path(__file__).with_name("test-web-release-acceptance.py")))
OfflineCase = fixtures["OfflineCase"]
interpreter_loader_env = fixtures["interpreter_loader_env"]
REPO_ROOT = fixtures["REPO_ROOT"]
REPOSITORY = fixtures["REPOSITORY"]
SHA = fixtures["SHA"]
RUN_ID = fixtures["RUN_ID"]
ATTEMPT = fixtures["ATTEMPT"]
WORKFLOW = fixtures["WORKFLOW"]
ARTIFACT_ID = fixtures["ARTIFACT_ID"]
ARTIFACT_NAME = fixtures["ARTIFACT_NAME"]
CANARY = fixtures["CANARY"]
STABLE = fixtures["STABLE"]
PLAN = fixtures["PLAN"]
PAYLOAD = fixtures["PAYLOAD"]
CONFIG = fixtures["CONFIG"]
manifest_fixture = fixtures["manifest_fixture"]
archive_fixture = fixtures["archive_fixture"]
evidence_fixture = fixtures["evidence_fixture"]
certify_fixture = fixtures["EvidenceTests"].certify


def sha256(data):
    return hashlib.sha256(data).hexdigest()


RAW_LOADER = b'import { Server } from "./../cloudflare-tmp/server.js"; export default new Server();\n'
BUNDLE = {
    "_worker.js": b'import app from "./modules/server.js"; export default app;\n',
    "modules/server.js": b'export default {fetch() { return new Response("complete graph"); }};\n',
    "modules/nested/opaque.js": b'export const retained = "nested";\n',
    "wasm/engine.wasm": b"\x00asm\x01\x00\x00\x00",
    ".module-map": b"hidden producer module map\n",
    "_worker.js.map": b'{"version":3,"file":"_worker.js","sources":["server.js"]}\n',
}


class ProducerModuleClosureTests(OfflineCase):
    def setUp(self):
        super().setUp()
        # Model the adapter's flat loader and its real sibling server, not
        # the already-self-contained module-directory fixture in the base bank.
        shutil.rmtree(self.root / "_worker.js")
        (self.root / "_worker.js").write_bytes(RAW_LOADER)
        sibling = self.tmp / "cloudflare-tmp"
        sibling.mkdir()
        (sibling / "server.js").write_bytes(b"not inside the retained assets root\n")
        (self.root / "assets/client.js").write_bytes(b"window.clientOnly = true;\n")
        self.config.write_text(CONFIG.replace("_worker.js/index.js", "_worker.js"))
        self.bundle = self.tmp / "bundle"
        self.bundle.mkdir()
        for path, data in BUNDLE.items():
            target = self.bundle / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)

    def package_bundle(self, bundle=None):
        return self.tool(
            "web-release-artifact.py", "package", "--root", self.root,
            "--bundle-root", self.bundle if bundle is None else bundle,
            "--config", self.config, "--plan", self.plan,
            "--repository", REPOSITORY, "--sha", SHA, "--run-id", RUN_ID,
            "--attempt", ATTEMPT, "--public-api-base-url", "", "--output", self.archive,
        )

    def expected_payload(self):
        static = {name: data for name, data in PAYLOAD.items() if not name.startswith("_worker.js/")}
        static["assets/client.js"] = b"window.clientOnly = true;\n"
        modules = {
            "_worker.js/" + ("index.js" if name == "_worker.js" else name): data
            for name, data in BUNDLE.items()
        }
        return {**static, **modules}

    def test_raw_sibling_server_loader_without_producer_bundle_refuses(self):
        for loader in [RAW_LOADER, b'export { default } from "../cloudflare-tmp/server.js";\n']:
            with self.subTest(loader=loader):
                (self.root / "_worker.js").write_bytes(loader)
                result = self.package()
                self.assertEqual(self.refusal(result)["code"], "producer-bundle-required")
                self.assertFalse(self.archive.exists())

    def test_complete_emitted_tree_is_retained_with_static_assets_and_only_main_renamed(self):
        self.success(self.package_bundle())
        expected = self.expected_payload()
        with tarfile.open(self.archive) as archive:
            members = {member.name for member in archive.getmembers() if member.isfile()}
            self.assertEqual(members, {"manifest.json", *("web/" + name for name in expected)})
            manifest = json.load(archive.extractfile("manifest.json"))
            self.assertEqual(manifest["plan"], PLAN)
            self.assertEqual(manifest["components"], ["web"])
            self.assertEqual(manifest["files"], [
                {"path": name, "size": len(data), "sha256": sha256(data)}
                for name, data in sorted(expected.items())
            ])
            for name, data in expected.items():
                self.assertEqual(archive.extractfile("web/" + name).read(), data)
            self.assertNotIn("web/_worker.js", members)
            self.assertNotIn("web/cloudflare-tmp/server.js", members)
        self.success(self.verify("--require-portable"))
        self.assertEqual(self.saved_payload(), expected)

    def test_additional_modules_stay_worker_scoped_and_rendering_never_rewrites_them(self):
        self.success(self.package_bundle())
        self.success(self.verify("--require-portable"))
        expected = self.expected_payload()
        saved_manifest = (self.destination / "manifest.json").read_bytes()
        for environment, version in [("staging", CANARY), ("production", STABLE)]:
            with self.subTest(environment=environment):
                self.rendered = self.tmp / (environment + ".toml")
                self.success(self.render(environment, version))
                import tomllib
                config = tomllib.loads(self.rendered.read_text())
                effective = {**config, **config.get("env", {}).get(environment, {})}
                base = (self.rendered.parent / effective["base_dir"]).resolve()
                self.assertEqual(base, self.destination / "web/_worker.js")
                self.assertIs(effective["find_additional_modules"], True)
                self.assertNotIn(self.destination / "web/assets/client.js", base.rglob("*.js"))
                self.assertEqual((self.rendered.parent / effective["main"]).resolve(), base / "index.js")
                self.assertEqual(effective["vars"], {"DEPLOYED_RELEASE_VERSION": version})
                self.assertEqual(self.saved_payload(), expected)
                self.assertEqual((self.destination / "manifest.json").read_bytes(), saved_manifest)

    def test_missing_emitted_entry_refuses(self):
        (self.bundle / "_worker.js").unlink()
        self.refusal(self.package_bundle())
        self.assertFalse(self.archive.exists())

    def test_ambiguous_worker_and_index_mapping_refuses_instead_of_overwriting_the_main(self):
        # Both names map to web/_worker.js/index.js. An accepted inventory can
        # otherwise conceal that the real emitted main was overwritten.
        (self.bundle / "index.js").write_bytes(b"ambiguous second main\n")
        self.refusal(self.package_bundle())
        self.assertFalse(self.archive.exists())

    def test_links_in_the_producer_bundle_cannot_escape_into_retained_modules(self):
        outside = self.tmp / "outside-module"
        outside.write_bytes(b"outside producer output\n")
        for kind in ["symlink", "hardlink"]:
            with self.subTest(kind=kind):
                link = self.bundle / "modules/escape.js"
                if kind == "symlink":
                    link.symlink_to(outside)
                else:
                    os.link(outside, link)
                try:
                    self.refusal(self.package_bundle())
                    self.assertFalse(self.archive.exists())
                finally:
                    link.unlink()
                self.assertEqual(outside.read_bytes(), b"outside producer output\n")


class MalformedBoundaryTests(OfflineCase):
    def structured_refusal(self, result):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        try:
            response = json.loads(result.stdout)
        except (ValueError, TypeError):
            self.fail("refusal must be a JSON object, not a traceback/setup failure: " + result.stdout + result.stderr)
        self.assertIsInstance(response, dict)
        self.assertIs(response.get("ok"), False)
        self.assertIsInstance(response.get("code"), str)
        self.assertTrue(response["code"].strip())

    def assert_no_extraction(self):
        if self.destination.exists():
            self.assertEqual([p.relative_to(self.destination).as_posix() for p in self.destination.rglob("*") if p.is_file()], [])

    def test_malformed_inventory_entries_refuse_structurally_without_extraction(self):
        for index, files in enumerate([[42], [None], ["entry"], [{}], {"path": "index.js"}, [False]]):
            with self.subTest(files=files):
                self.destination = self.tmp / ("bad-inventory-" + str(index))
                manifest = manifest_fixture()
                manifest["files"] = files
                archive_fixture(self.archive, manifest)
                self.structured_refusal(self.verify())
                self.assert_no_extraction()
                self.assertFalse((self.destination / "web/_worker.js/index.js").exists())

    def test_nonobject_manifest_fields_refuse_without_any_usable_output(self):
        mutants = [(key, value) for key in ["source", "compatibility", "build", "plan"] for value in [None, 42, "wrong", []]]
        for index, (key, value) in enumerate(mutants):
            with self.subTest(key=key, value=value):
                self.destination = self.tmp / ("bad-field-" + str(index))
                manifest = manifest_fixture()
                manifest[key] = value
                archive_fixture(self.archive, manifest)
                self.structured_refusal(self.verify())
                self.assert_no_extraction()
        for value in [42, [], "manifest is not an object"]:
            archive_fixture(self.archive, value)
            self.destination = self.tmp / ("bad-root-" + str(len(str(value))))
            self.structured_refusal(self.verify())
            self.assert_no_extraction()

    def test_nested_nonobject_evidence_refuses_without_a_certificate(self):
        selectors = [
            ("run",), ("artifact",), ("jobPages",), ("jobPages", 0),
            ("jobPages", 0, "jobs", 0), ("jobPages", 0, "jobs", 2, "steps", 0),
            ("run", "repository"), ("run", "head_repository"), ("artifact", "workflow_run"),
        ]
        for selector in selectors:
            for value in [None, 42, "wrong"]:
                with self.subTest(selector=selector, value=value):
                    evidence = evidence_fixture()
                    cursor = evidence
                    for part in selector[:-1]:
                        cursor = cursor[part]
                    cursor[selector[-1]] = value
                    result = certify_fixture(self, evidence)
                    self.structured_refusal(result)
                    self.assertFalse(self.certificate.exists())

    def test_nonobject_certificate_manifest_fields_refuse_without_a_certificate(self):
        for key in ["source", "plan"]:
            for value in [None, 42, [], "wrong"]:
                with self.subTest(key=key, value=value):
                    manifest = manifest_fixture()
                    manifest[key] = value
                    self.structured_refusal(certify_fixture(self, evidence_fixture(), manifest))
                    self.assertFalse(self.certificate.exists())


# Replace only urllib's byte-I/O handler: real Request, opener, error and
# redirect policies execute. An unexpected endpoint fails closed and every
# request is retained, including any Authorization forwarded to a signed URL.
FAKE_HTTP_RUNNER = '''import base64, io, json, runpy, socket, sys
from email.message import Message
from pathlib import Path
import urllib.parse, urllib.request, urllib.response
scenario_path, audit_path, script = sys.argv[1:4]
scenario = json.loads(Path(scenario_path).read_text())
audit = []
def key(url):
    parsed = urllib.parse.urlsplit(url)
    return (parsed.scheme, parsed.netloc, parsed.path, tuple(sorted(urllib.parse.parse_qsl(parsed.query))))
def response(self, request):
    audit.append({"url": request.full_url, "method": request.get_method(), "headers": {k.lower(): v for k, v in request.header_items()}})
    Path(audit_path).write_text(json.dumps(audit))
    if len(audit) > 32:
        raise RuntimeError("fake HTTP request bound exceeded")
    row = next((item for item in scenario if key(item["url"]) == key(request.full_url)), None)
    if row is None:
        row = {"status": 404, "body": base64.b64encode(b'{"message":"unexpected fake endpoint"}').decode()}
    headers = Message()
    for name, value in row.get("headers", {}).items():
        headers[name] = value
    result = urllib.response.addinfourl(io.BytesIO(base64.b64decode(row["body"])), headers, request.full_url, row.get("status", 200))
    result.msg = "fake HTTP response"
    return result
def refuse(*args, **kwargs):
    raise RuntimeError("real socket forbidden in fake HTTP acceptance")
urllib.request.HTTPSHandler.https_open = response
urllib.request.HTTPHandler.http_open = response
socket.socket = refuse
socket.create_connection = refuse
sys.argv = [script, *sys.argv[4:]]
sys.path.insert(0, str(Path(script).resolve().parent))
runpy.run_path(script, run_name="__main__")
'''


class GitHubTransportTests(OfflineCase):
    def setUp(self):
        super().setUp()
        self.api = "https://api.github.com/repos/" + REPOSITORY
        self.run_url = self.api + f"/actions/runs/{RUN_ID}/attempts/{ATTEMPT}"
        self.artifact_url = self.api + f"/actions/artifacts/{ARTIFACT_ID}"
        self.jobs_url = self.api + f"/actions/runs/{RUN_ID}/attempts/{ATTEMPT}/jobs?per_page=100&page=1"
        self.download_url = self.artifact_url + "/zip"
        self.signed_url = "https://release-fixture.blob.core.windows.net/signed.zip?signature=fixture-only"
        self.metadata_output = self.tmp / "fetched.json"
        self.archive_output = self.tmp / "downloaded.tar.gz"
        self.audit_path = self.tmp / "http-audit.json"
        self.resolve_output = self.tmp / "resolved.json"
        self.canary_tag = "canary/v" + CANARY
        self.tag_sha = "b" * 40
        self.tag_ref_url = self.api + "/git/ref/tags/" + urllib.parse.quote(self.canary_tag, safe="")
        self.tag_url = self.api + "/git/tags/" + self.tag_sha
        archive_fixture(self.archive)
        self.retained_bytes = self.archive.read_bytes()
        self.zip_bytes = self.make_zip([("web-release.tar.gz", self.retained_bytes)])
        self.fixture = evidence_fixture()
        self.fixture["artifact"]["digest"] = "sha256:" + sha256(self.zip_bytes)

    @staticmethod
    def make_zip(members):
        import warnings
        data = io.BytesIO()
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", UserWarning)
            with zipfile.ZipFile(data, "w", compression=zipfile.ZIP_STORED) as archive:
                for name, content in members:
                    archive.writestr(name, content)
        return data.getvalue()

    @staticmethod
    def http_row(url, body, status=200, headers=None):
        if not isinstance(body, bytes):
            body = json.dumps(body).encode()
        return {"url": url, "status": status, "headers": headers or {}, "body": base64.b64encode(body).decode()}

    def http_tool(self, args, rows):
        scenario = self.tmp / "fake-http.json"
        scenario.write_text(json.dumps(rows))
        self.audit_path.write_text("[]")
        return subprocess.run(
            [sys.executable, "-I", "-c", FAKE_HTTP_RUNNER, str(scenario), str(self.audit_path),
             str(REPO_ROOT / "ci/web-release-evidence.py"), *map(str, args)],
            cwd=self.tmp,
            env={"PATH": "", "GITHUB_TOKEN": "offline-fixture-token", "PYTHONDONTWRITEBYTECODE": "1", **interpreter_loader_env()},
            text=True, capture_output=True, timeout=15,
        )

    def requests(self):
        return json.loads(self.audit_path.read_text())

    def fetch_rows(self, fixture=None, download=None):
        if fixture is None:
            fixture = self.fixture
        if download is None:
            download = self.zip_bytes
        jobs = fixture["jobPages"][0]["jobs"]
        return [
            self.http_row(self.run_url, fixture["run"]),
            self.http_row(self.artifact_url, fixture["artifact"]),
            self.http_row(self.jobs_url, {"total_count": len(jobs), "jobs": jobs}),
            self.http_row(self.download_url, download),
        ]

    def fetch(self, rows=None):
        return self.http_tool([
            "fetch", "--repository", REPOSITORY, "--run-id", RUN_ID, "--attempt", ATTEMPT,
            "--artifact-id", ARTIFACT_ID, "--artifact-name", ARTIFACT_NAME,
            "--artifact-digest", self.fixture["artifact"]["digest"],
            "--archive-output", self.archive_output, "--output", self.metadata_output,
        ], self.fetch_rows() if rows is None else rows)

    def assert_no_download(self, result):
        self.refusal(result)
        self.assertNotIn("Traceback", result.stderr)
        self.assertFalse(self.archive_output.exists())
        self.assertFalse(self.metadata_output.exists())

    def certify_fetched(self):
        manifest = self.tmp / "verified-manifest.json"
        manifest.write_text(json.dumps(manifest_fixture()))
        self.certificate = self.tmp / "certificate.json"
        return self.tool(
            "web-release-evidence.py", "certify", "--input", self.metadata_output,
            "--manifest", manifest, "--repository", REPOSITORY, "--sha", SHA,
            "--run-id", RUN_ID, "--attempt", ATTEMPT, "--workflow", WORKFLOW,
            "--artifact-id", ARTIFACT_ID, "--artifact-name", ARTIFACT_NAME,
            "--artifact-digest", self.fixture["artifact"]["digest"], "--output", self.certificate,
            "--require-completed",
        )

    def annotation(self):
        evidence = {
            "runId": RUN_ID, "attempt": ATTEMPT, "workflow": WORKFLOW,
            "jobs": fixtures["REQUIRED_JOBS"], "artifactId": ARTIFACT_ID,
            "artifactName": ARTIFACT_NAME, "artifactDigest": self.fixture["artifact"]["digest"],
        }
        return {**copy.deepcopy(PLAN), "publicationEvidence": evidence}

    def resolve_rows(self, annotation=None, ref=None, tag=None):
        if annotation is None:
            annotation = self.annotation()
        if ref is None:
            ref = {"ref": "refs/tags/" + self.canary_tag, "object": {"type": "tag", "sha": self.tag_sha}}
        if tag is None:
            tag = {"sha": self.tag_sha, "tag": self.canary_tag, "object": {"type": "commit", "sha": SHA},
                   "message": "calver-plan-v1\n" + json.dumps(annotation)}
        return [self.http_row(self.tag_ref_url, ref), self.http_row(self.tag_url, tag)]

    def resolve(self, rows=None):
        return self.http_tool([
            "resolve", "--repository", REPOSITORY, "--canary", self.canary_tag,
            "--sha", SHA, "--output", self.resolve_output,
        ], self.resolve_rows() if rows is None else rows)

    def test_fake_http_interpreter_starts_without_inheriting_parent_credentials(self):
        loader_path = os.pathsep.join(filter(None, [
            os.environ.get("LD_LIBRARY_PATH"), str(self.tmp / "python-loader"),
        ]))
        with mock.patch.dict(os.environ, {
            "LD_LIBRARY_PATH": loader_path, "PATH": "parent-path-must-not-leak",
            "GITHUB_TOKEN": "parent-token-must-not-leak", "GH_TOKEN": "parent-token-must-not-leak",
            "PYTHONPATH": "parent-modules-must-not-leak", "PYTHONHOME": "parent-home-must-not-leak",
        }, clear=True), mock.patch("subprocess.run", wraps=subprocess.run) as spawn:
            result = self.http_tool(["--help"], [])
        self.assertEqual(spawn.call_args.kwargs["env"], {
            "PATH": "", "GITHUB_TOKEN": "offline-fixture-token", "PYTHONDONTWRITEBYTECODE": "1",
            "LD_LIBRARY_PATH": loader_path,
        })
        self.assertEqual(spawn.call_args.args[0][:3], [sys.executable, "-I", "-c"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("usage:", result.stdout)
        self.assertEqual(self.requests(), [])

    def test_fetch_reads_only_the_exact_attempt_and_returns_the_exact_retained_payload(self):
        self.success(self.fetch())
        self.assertEqual(self.archive_output.read_bytes(), self.retained_bytes)
        envelope = json.loads(self.metadata_output.read_text())
        self.assertEqual(envelope, self.fixture)
        urls = [request["url"] for request in self.requests()]
        self.assertEqual(set(urls), {self.run_url, self.artifact_url, self.jobs_url, self.download_url})
        self.assertEqual(len(urls), 4)
        self.assertTrue(all(request["method"] == "GET" for request in self.requests()))
        self.success(self.certify_fetched())
        self.assertEqual(json.loads(self.certificate.read_text())["components"], ["web"])

    def test_fetch_collects_all_linked_count_complete_job_pages(self):
        required = self.fixture["jobPages"][0]["jobs"]
        unrelated = [fixtures["job_fixture"]("other-job-" + str(i), 10000 + i, []) for i in range(98)]
        first_jobs = [*required[:2], *unrelated]
        last_jobs = required[2:]
        second = self.jobs_url.replace("&page=1", "&page=2")
        rows = self.fetch_rows()
        rows[2] = self.http_row(self.jobs_url, {"total_count": 101, "jobs": first_jobs},
                                headers={"Link": '<' + second + '>; rel="next"'})
        rows.append(self.http_row(second, {"total_count": 101, "jobs": last_jobs}))
        self.success(self.fetch(rows))
        pages = json.loads(self.metadata_output.read_text())["jobPages"]
        self.assertEqual(pages, [
            {"page": 1, "total_count": 101, "nextPage": 2, "jobs": first_jobs},
            {"page": 2, "total_count": 101, "nextPage": None, "jobs": last_jobs},
        ])
        self.success(self.certify_fetched())
        self.assertIn(second, [request["url"] for request in self.requests()])

    def test_count_truncation_or_duplicate_jobs_cannot_mint_a_certificate(self):
        jobs = self.fixture["jobPages"][0]["jobs"]
        mutants = [
            {"total_count": 4, "jobs": jobs},
            {"total_count": 3, "jobs": jobs[:2]},
            {"total_count": 4, "jobs": [*jobs, jobs[0]]},
        ]
        for index, page in enumerate(mutants):
            with self.subTest(page=page):
                self.metadata_output = self.tmp / ("count-" + str(index) + ".json")
                self.archive_output = self.tmp / ("count-" + str(index) + ".tar.gz")
                rows = self.fetch_rows()
                rows[2] = self.http_row(self.jobs_url, page)
                result = self.fetch(rows)
                if result.returncode == 0:
                    result = self.certify_fetched()
                self.refusal(result)
                self.assertFalse((self.tmp / "certificate.json").exists())

    def test_foreign_or_other_attempt_metadata_never_certifies_even_on_the_same_sha(self):
        mutations = [
            ("run", "id", RUN_ID + 1), ("run", "run_attempt", ATTEMPT + 1),
            ("run", "event", "workflow_dispatch"),
            ("run", "repository", {"id": 100, "full_name": "other/repository"}),
            ("artifact", "expired", True), ("artifact", "id", ARTIFACT_ID + 1),
            ("artifact", "name", ARTIFACT_NAME + "-rebuilt"),
            ("artifact", "workflow_run", {"id": RUN_ID + 1, "head_sha": SHA}),
        ]
        for index, (section, key, value) in enumerate(mutations):
            with self.subTest(section=section, key=key):
                fixture = copy.deepcopy(self.fixture)
                fixture[section][key] = value
                self.metadata_output = self.tmp / ("foreign-" + str(index) + ".json")
                self.archive_output = self.tmp / ("foreign-" + str(index) + ".tar.gz")
                result = self.fetch(self.fetch_rows(fixture))
                if result.returncode == 0:
                    result = self.certify_fetched()
                self.refusal(result)
                self.assertFalse((self.tmp / "certificate.json").exists())

    def test_download_verifies_the_github_zip_digest_not_the_inner_tar_digest(self):
        # Valid membership is not enough: the exact uploaded GitHub ZIP bytes
        # are bound independently from every tar/member hash.
        self.assert_no_download(self.fetch(self.fetch_rows(download=self.zip_bytes + b"changed archive bytes")))

    def test_unapproved_zip_membership_refuses_even_with_a_matching_zip_digest(self):
        cases = [
            [("other.tar.gz", self.retained_bytes)],
            [("web-release.tar.gz", self.retained_bytes), ("web-release.tar.gz", b"second payload")],
            [("../web-release.tar.gz", self.retained_bytes)],
            [("nested/web-release.tar.gz", self.retained_bytes)],
            [("web-release.tar.gz", self.retained_bytes), ("extra.txt", b"unlisted")],
        ]
        for index, members in enumerate(cases):
            with self.subTest(names=[name for name, _ in members]):
                archive = self.make_zip(members)
                self.fixture["artifact"]["digest"] = "sha256:" + sha256(archive)
                self.archive_output = self.tmp / ("zip-" + str(index) + ".tar.gz")
                self.metadata_output = self.tmp / ("zip-" + str(index) + ".json")
                self.assert_no_download(self.fetch(self.fetch_rows(download=archive)))

    def test_existing_archive_output_is_not_overwritten(self):
        self.archive_output.write_bytes(b"keep existing output")
        self.refusal(self.fetch())
        self.assertEqual(self.archive_output.read_bytes(), b"keep existing output")
        self.assertFalse(self.metadata_output.exists())

    def test_signed_redirect_fetches_bytes_without_forwarding_github_authorization(self):
        rows = self.fetch_rows()
        rows[-1] = self.http_row(self.download_url, b"", 302, {"Location": self.signed_url})
        rows.append(self.http_row(self.signed_url, self.zip_bytes))
        self.success(self.fetch(rows))
        signed = [request for request in self.requests() if request["url"] == self.signed_url]
        self.assertEqual(len(signed), 1)
        self.assertNotIn("authorization", signed[0]["headers"])
        api = [request for request in self.requests() if urllib.parse.urlsplit(request["url"]).hostname == "api.github.com"]
        self.assertTrue(api)
        self.assertTrue(all(request["headers"].get("authorization") == "Bearer offline-fixture-token" for request in api))
        self.assertEqual(self.archive_output.read_bytes(), self.retained_bytes)

    def test_insecure_or_missing_signed_location_refuses_without_a_download(self):
        local_fixture = self.tmp / "local-redirect.zip"
        local_fixture.write_bytes(self.zip_bytes)
        for index, location in enumerate(["http://artifact.example.test/payload", "https://artifact.example.test/untrusted", "/relative-only", "", local_fixture.as_uri()]):
            with self.subTest(location=location):
                self.archive_output = self.tmp / ("redirect-" + str(index) + ".tar.gz")
                self.metadata_output = self.tmp / ("redirect-" + str(index) + ".json")
                rows = self.fetch_rows()
                rows[-1] = self.http_row(self.download_url, b"", 302, {"Location": location})
                rows.append(self.http_row(location, self.zip_bytes))
                self.assert_no_download(self.fetch(rows))
                foreign = [request for request in self.requests() if urllib.parse.urlsplit(request["url"]).hostname != "api.github.com"]
                self.assertTrue(all("authorization" not in request["headers"] for request in foreign))

    def test_malformed_fetch_json_and_invalid_zip_bytes_refuse_without_a_certificate(self):
        for index, (row_index, value) in enumerate([(0, 42), (1, []), (2, {"total_count": 1, "jobs": [42]})]):
            with self.subTest(row_index=row_index):
                self.archive_output = self.tmp / ("malformed-fetch-" + str(index) + ".tar.gz")
                self.metadata_output = self.tmp / ("malformed-fetch-" + str(index) + ".json")
                rows = self.fetch_rows()
                rows[row_index] = self.http_row(rows[row_index]["url"], value)
                result = self.fetch(rows)
                if result.returncode == 0:
                    result = self.certify_fetched()
                self.refusal(result)
                self.assertNotIn("Traceback", result.stderr)
                self.assertFalse((self.tmp / "certificate.json").exists())
        content = b"a digest-matching body that is not a ZIP archive"
        self.fixture["artifact"]["digest"] = "sha256:" + sha256(content)
        self.archive_output = self.tmp / "not-a-zip.tar.gz"
        self.metadata_output = self.tmp / "not-a-zip.json"
        self.assert_no_download(self.fetch(self.fetch_rows(download=content)))

    def test_malformed_tag_json_refuses_without_a_resolved_selector(self):
        rows = [
            [self.http_row(self.tag_ref_url, 42)],
            [self.resolve_rows()[0], self.http_row(self.tag_url, [])],
            [self.resolve_rows()[0], self.http_row(self.tag_url, {
                "sha": self.tag_sha, "tag": self.canary_tag,
                "object": {"type": "commit", "sha": SHA}, "message": "calver-plan-v1\n[42]",
            })],
        ]
        for index, scenario in enumerate(rows):
            with self.subTest(case=index):
                self.resolve_output = self.tmp / ("malformed-tag-" + str(index) + ".json")
                result = self.resolve(scenario)
                self.refusal(result)
                self.assertNotIn("Traceback", result.stderr)
                self.assertFalse(self.resolve_output.exists())

    def test_resolve_preserves_exact_frozen_plan_and_publication_evidence(self):
        annotation = self.annotation()
        self.success(self.resolve())
        self.assertEqual(json.loads(self.resolve_output.read_text()), {
            "plan": PLAN, "evidence": annotation["publicationEvidence"],
        })
        self.assertEqual([request["url"] for request in self.requests()], [self.tag_ref_url, self.tag_url])
        self.assertTrue(all(request["method"] == "GET" for request in self.requests()))

    def test_resolve_rejects_foreign_sha_repository_and_unbound_annotation(self):
        mutants = []
        wrong_sha = self.annotation()
        wrong_sha["sha"] = "f" * 40
        mutants.append(self.resolve_rows(wrong_sha))
        foreign = self.annotation()
        foreign["repository"] = "other/repository"
        mutants.append(self.resolve_rows(foreign))
        no_proof = self.annotation()
        del no_proof["publicationEvidence"]
        mutants.append(self.resolve_rows(no_proof))
        mutants.append(self.resolve_rows(tag={"object": {"type": "commit", "sha": "f" * 40}, "message": "legacy tag"}))
        mutants.append(self.resolve_rows(ref={"object": {"type": "commit", "sha": SHA}}))
        for index, rows in enumerate(mutants):
            with self.subTest(case=index):
                self.resolve_output = self.tmp / ("bad-canary-" + str(index) + ".json")
                self.refusal(self.resolve(rows))
                self.assertFalse(self.resolve_output.exists())


if __name__ == "__main__":
    unittest.main()
