from pathlib import Path
import subprocess
import tempfile
import unittest

from ci_coverage import SELECTABLE, plan
from test_ci_pnpm_retry import job_level_field, split_jobs


class CoverageTests(unittest.TestCase):
    def test_coverage_from_git_pairs(self):
        def git(*args):
            return subprocess.check_output(["git", *args], cwd=root, text=True).strip()

        def write(path, data="synthetic\n"):
            target = root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(data, encoding="utf-8")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            git("init", "-q")
            git("config", "user.name", "Test")
            git("config", "user.email", "test@example.invalid")
            write("workers/api/src/old.rs")
            input_docs = ("docs/api-errors.md", "docs/adr/0004-plan-matrix.md", "docs/logging-standard.md")
            write("workers/mb/tests/doc_inputs.rs",
                  "\n".join(f'include_str!("../../../{doc}");' for doc in input_docs))
            git("add", ".")
            git("commit", "-qm", "Base")
            base = git("rev-parse", "HEAD")
            base_tree = git("rev-parse", "HEAD^{tree}")
            for files, includes, excludes in (
                (["docs/safe.md", "LICENSE"], [], SELECTABLE),
                (["workers/stream/src/api_error.rs"], ["rust-wasm", "rust-test", "rust-mb"], ["web-checks"]),
                (["workers/repair/src/runtime.rs"], ["rust-repair", "rust-ingest"], ["web-checks"]),
                (["workers/youtube/src/lib.rs"], ["worker-wasm-budget", "rust-wasm"], ["web-checks"]),
                (["apps/web/tests/new.spec.ts"], ["web-checks", "web-build", "web-e2e"],
                 ["api-migrations", "worker-release-builds"]),
                (["migrations/9999_synthetic.sql"], ["api-migrations", "mb-import", "worker-release-builds"],
                 ["web-checks"]),
                (["workers/ingest/src/new.rs", "apps/web/src/new.ts"],
                 ["api", "api-migrations", "rust-ingest", "rust-test", "rust-mb", "web-checks"], []),
                *[([path], SELECTABLE, []) for path in
                  (".github/workflows/new.yml", "Cargo.lock", "apps/web/package.json",
                   "workers/api/Cargo.toml", "crates/shared/src/lib.rs", "docs/synthetic.json",
                   "fixtures/synthetic.sql",
                   "docs/nested/pnpm-lock.yaml", "new-area/code.ts", *input_docs)],
            ):
                with self.subTest(files=files):
                    git("reset", "--hard", base)
                    for path in files:
                        write(path)
                    git("add", ".")
                    git("commit", "-qm", "Test path")
                    tested = git("rev-parse", "HEAD")
                    # Use the committed trees, including with a dirty checkout.
                    write("uncommitted")
                    complete, _, skip = plan("pull_request", base, tested, root)
                    self.assertTrue(complete)
                    self.assertTrue(set(includes).isdisjoint(skip), skip)
                    self.assertTrue(set(excludes) <= set(skip), skip)
                    self.assertEqual(plan("schedule", base, tested, root), (True, {"full"}, []))
                    if files == ["docs/safe.md", "LICENSE"]:
                        self.assertEqual(plan("pull_request", base_tree, tested, root),
                                         (False, {"incomplete (full coverage required)"}, []))
                    (root / "uncommitted").unlink()

            git("reset", "--hard", base)
            (root / "docs").mkdir(exist_ok=True)
            git("mv", "workers/api/src/old.rs", "docs/moved.md")
            git("commit", "-qm", "Move source to docs")
            complete, _, skip = plan("pull_request", base, git("rev-parse", "HEAD"), root)
            self.assertTrue(complete)
            self.assertNotIn("api-migrations", skip)
            self.assertNotIn("rust-test", skip)
            self.assertEqual(plan("pull_request", "a" * 40, base, root),
                             (False, {"incomplete (full coverage required)"}, []))
            write("docs/tool.md")
            git("add", "--chmod=+x", "docs/tool.md")
            git("commit", "-qm", "Add executable docs path")
            self.assertEqual(plan("pull_request", base, git("rev-parse", "HEAD"), root),
                             (True, {"full"}, []))

    def test_job_selection_and_dependencies(self):
        workflow = Path(__file__).resolve().parents[1] / "workflows/ci.yml"
        jobs = split_jobs(workflow.read_text(encoding="utf-8"))
        gated = {job for job, source in jobs.items()
                 if "fromJSON(needs.early.outputs.skip" in source}
        self.assertEqual(gated, SELECTABLE)
        for job in gated:
            with self.subTest(job=job):
                self.assertIn(f"'{job}'", job_level_field(jobs[job], "if"))
                self.assertIn("early", job_level_field(jobs[job], "needs"))
        self.assertIn("web-build", job_level_field(jobs["web-e2e"], "needs"))
        self.assertIn("worker-wasm-budget", job_level_field(jobs["worker-release-builds"], "needs"))


if __name__ == "__main__":
    unittest.main()
