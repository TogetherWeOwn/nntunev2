"""Synthetic preparation and authenticated Actions response fixtures."""

from copy import deepcopy
from datetime import datetime, timedelta, timezone
import io
from pathlib import Path
import stat
import tempfile
import unittest
import zipfile

from staging_admission import Refused, digest
from staging_api_candidate import CI_WORKFLOW, DRY_RUN, JOB, PREPARE, REPOSITORY, UPLOAD, prepare, verified_candidate

SHA = "a" * 40
REPO_ID = 10
RUN = 20
WORKFLOW = 30
FILES = {
    "index.js": b"import wasm from './index_bg.wasm';",
    "index_bg.wasm": b"\0asm\1\0\0\0",
    "package.json": b'{"main":"index.js","sideEffects":["./index.js"]}',
    "worker/shim.mjs": b"export {default} from '../index.js';",
}


def make_archive(files):
    result = io.BytesIO()
    with zipfile.ZipFile(result, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, data in files.items():
            archive.writestr(name, data)
    return result.getvalue()


class ActionsFixture:
    def __init__(self):
        repo = {"full_name": REPOSITORY, "id": REPO_ID}
        self.run = {
            "id": RUN, "workflow_id": WORKFLOW, "path": CI_WORKFLOW, "event": "push",
            "head_branch": "main", "head_sha": SHA, "run_attempt": 1,
            "status": "in_progress", "conclusion": None, "repository": repo, "head_repository": repo,
        }
        self.runs = [{"id": RUN}]
        self.jobs = [{
            "name": JOB, "run_id": RUN, "run_attempt": 1, "head_sha": SHA,
            "status": "completed", "conclusion": "success",
            "steps": [{"name": name, "status": "completed", "conclusion": "success", "number": i}
                      for i, name in enumerate(("Build ordinary Worker release bundles", DRY_RUN, PREPARE, UPLOAD), 1)],
        }]
        self.artifacts = [{
            "id": 40, "name": f"staging-api-ordinary-v1-{SHA}-{RUN}-1", "expired": False,
            "expires_at": (datetime.now(timezone.utc) + timedelta(days=1)).isoformat(),
            "workflow_run": {"id": RUN, "repository_id": REPO_ID, "head_repository_id": REPO_ID,
                             "head_branch": "main", "head_sha": SHA},
        }]
        self.set_archive(make_archive(FILES))
        self.downloads = 0
        self.supersede_after_download = False

    def set_archive(self, data):
        self.archive = data
        self.artifacts[0].update(size_in_bytes=len(data), digest="sha256:" + digest(data))

    def api(self, path):
        if path == f"runs/{RUN}" and self.downloads and self.supersede_after_download:
            return self.run | {"run_attempt": 2}
        return deepcopy({
            "workflows/ci.yml": {"id": WORKFLOW, "path": CI_WORKFLOW},
            f"runs/{RUN}": self.run,
        }[path])

    def listing(self, path, key):
        return deepcopy({
            (f"workflows/{WORKFLOW}/runs?branch=main&event=push&head_sha={SHA}", "workflow_runs"): self.runs,
            (f"runs/{RUN}/attempts/1/jobs", "jobs"): self.jobs,
            (f"runs/{RUN}/artifacts", "artifacts"): self.artifacts,
        }[path, key])

    def read(self, path, limit):
        assert path == "artifacts/40/zip" and len(self.archive) <= limit
        self.downloads += 1
        return self.archive


def verify(actions, mode="0"):
    return verified_candidate(actions, repository_id=REPO_ID, sha=SHA, private_trial_upload=mode)


class ApiCandidateTests(unittest.TestCase):
    def test_prepares_only_the_runtime_bundle_and_verifies_the_same_bytes(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            build = root / "build"
            (build / "worker").mkdir(parents=True)
            for name, data in (FILES | {"index_bg.wasm.map": b"source-map fixture"}).items():
                (build / name).write_bytes(data)
            payload = root / "payload"
            prepare(build, payload)
            files = {path.relative_to(payload).as_posix(): path.read_bytes()
                     for path in payload.rglob("*") if path.is_file()}
            actions = ActionsFixture()
            actions.set_archive(make_archive(files))
            self.assertEqual(verify(actions), FILES)
            self.assertEqual(actions.downloads, 1)
            (build / "index.js").unlink()
            (build / "index.js").symlink_to(build / "package.json")
            with self.assertRaisesRegex(Refused, "regular build output"):
                prepare(build, root / "refused")
            self.assertFalse((root / "refused").exists())

    def test_refuses_armed_failed_stale_or_missing_candidate_without_downloading(self):
        cases = (
            ("1", lambda a: None, "unavailable for this mode"),
            ("0", lambda a: a.runs.clear(), "missing or ambiguous candidate CI run"),
            ("0", lambda a: a.run.update(head_sha="b" * 40), "wrong or superseded CI attempt"),
            ("0", lambda a: a.run.update(event="pull_request"), "wrong or superseded CI attempt"),
            ("0", lambda a: a.run.update(run_attempt=2), "wrong or superseded CI attempt"),
            ("0", lambda a: a.jobs[0].update(conclusion="failure"), "Worker release build did not pass"),
            ("0", lambda a: a.jobs[0]["steps"][1].update(conclusion="skipped"), "required candidate build steps"),
            ("0", lambda a: a.jobs[0]["steps"].append(None), "invalid candidate CI step metadata"),
            ("0", lambda a: a.artifacts.clear(), "missing or ambiguous API build artifact"),
            ("0", lambda a: a.artifacts.extend(a.artifacts[:]), "missing or ambiguous API build artifact"),
            ("0", lambda a: a.artifacts[0].update(name=f"staging-api-ordinary-v1-{SHA}-{RUN}-2"),
             "missing or ambiguous API build artifact"),
            ("0", lambda a: a.artifacts[0].update(expires_at="2026-01-01T00:00:00Z"), "expired API build artifact"),
            ("0", lambda a: a.artifacts[0]["workflow_run"].update(head_sha="b" * 40), "invalid or expired"),
        )
        for mode, change, reason in cases:
            with self.subTest(reason=reason):
                actions = ActionsFixture()
                change(actions)
                with self.assertRaisesRegex(Refused, reason):
                    verify(actions, mode)
                self.assertEqual(actions.downloads, 0)

    def test_refuses_digest_mismatch_and_an_attempt_superseded_during_download(self):
        actions = ActionsFixture()
        actions.artifacts[0]["digest"] = "sha256:" + "0" * 64
        with self.assertRaisesRegex(Refused, "digest mismatch"):
            verify(actions)
        actions = ActionsFixture()
        actions.supersede_after_download = True
        with self.assertRaisesRegex(Refused, "superseded CI attempt"):
            verify(actions)

    def test_rejects_incomplete_oversized_or_nonregular_archive_members(self):
        symlink = zipfile.ZipInfo("index.js")
        symlink.create_system = 3
        symlink.external_attr = (stat.S_IFLNK | 0o777) << 16
        base = {name: data for name, data in FILES.items() if name != "index.js"}
        for files in (base, base | {"../index.js": b"unexpected"},
                      FILES | {"index.js": b""}, FILES | {"worker/shim.mjs": b"x" * 4097},
                      base | {symlink: b"package.json"}):
            with self.subTest(names=list(files)):
                actions = ActionsFixture()
                actions.set_archive(make_archive(files))
                with self.assertRaisesRegex(Refused, "build archive"):
                    verify(actions)


if __name__ == "__main__":
    unittest.main()
