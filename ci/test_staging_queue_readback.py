import json
from pathlib import Path
import subprocess
import sys
import unittest


STATES = ("queued", "pending", "requested", "waiting", "in_progress")
SCRIPT = Path(__file__).with_name("staging_queue_readback.py")
PATH = ".github/workflows/deploy.yml"
REPO = {"full_name": "TogetherWeOwn/nntunev2", "id": 71}


def run(rid, **overrides):
    return {"id": rid, "run_number": rid, "status": "pending", "event": "push",
            "head_branch": "main", "run_attempt": 1, "head_sha": f"{rid:040x}",
            "path": PATH, "workflow_id": 11, "repository": REPO, "head_repository": REPO,
            "conclusion": None} | overrides


def job(rid, jid=1, **overrides):
    return {"id": jid, "run_id": rid, "run_attempt": 1, "head_sha": f"{rid:040x}",
            "run_url": f"https://api.github.com/repos/TogetherWeOwn/nntunev2/actions/runs/{rid}",
            "status": "queued", "started_at": None, "completed_at": None,
            "conclusion": None, "runner_id": 0, "steps": []} | overrides


def pages(key, rows):
    return [{"total_count": len(rows), key: rows[start:start + 100]}
            for start in range(0, len(rows) or 1, 100)]


def snapshot(*runs, jobs=None):
    return {"workflow": {"path": PATH, "id": 11, "state": "active"},
            "runs": {state: pages("workflow_runs", [row for row in runs if row["status"] == state])
                     for state in STATES},
            "jobs": {f"runs/{rid}/attempts/1/jobs": pages("jobs", rows) for rid, rows in (jobs or {}).items()}}


class QueueReadbackTests(unittest.TestCase):
    def invoke(self, evidence, *pins):
        data = evidence if isinstance(evidence, str) else json.dumps(evidence)
        result = subprocess.run([sys.executable, "-I", str(SCRIPT), *pins], input=data,
                                text=True, capture_output=True, check=False, timeout=10)
        self.assertEqual(result.stderr, "")
        return result.returncode, json.loads(result.stdout)

    def assert_indeterminate(self, evidence, *pins):
        code, report = self.invoke(evidence, *pins)
        self.assertEqual(code, 1)
        self.assertEqual(report["status"], "indeterminate")
        self.assertEqual(set(report), {"status", "reason"})

    def test_keeps_newest_underway_dispatches_and_pins(self):
        records = [run(101, run_number=1), run(102, run_number=2), run(103, run_number=3),
                   run(104, run_number=4), run(105, run_number=5), run(6, status="queued"),
                   run(107, event="workflow_dispatch", display_title="Protected staging cleanup"),
                   run(108, event="workflow_dispatch"), run(109, event="repository_dispatch"),
                   run(110, status="in_progress"), run(111, head_branch="other"),
                   run(112, run_attempt=2), run(113, status="waiting"),
                   run(114, head_repository={"full_name": "Other/nntunev2", "id": 72})]
        evidence = snapshot(*records, jobs={101: [], 102: [], 103: [], 104: [job(104)], 6: [],
                                            105: [job(105, status="completed", conclusion="success"),
                                                  job(105, 2, name="post-deploy full journey")]})
        code, report = self.invoke(evidence, "--pin-run", "102", "--pin-sha", records[2]["head_sha"],
                                   "--pin-run", "907", "--pin-run", "908", "--pin-sha", "b" * 40)
        self.assertEqual(code, 0)
        self.assertEqual(report["status"], "complete")
        self.assertEqual(report["newest_eligible_run_id"], 6)
        self.assertEqual(report["unlisted_pins"], {"run_ids": [907, 908], "head_shas": ["b" * 40]})
        self.assertEqual({row["run_id"]: row["disposition"] for row in report["runs"]}, {
            101: "superseded", 102: "keep_pinned", 103: "keep_pinned", 104: "keep_existing_jobs",
            105: "keep_existing_jobs", 6: "keep_newest_eligible",
            **dict.fromkeys(range(107, 115), "keep_outside_selection"),
        })

    def test_admitted_gate_is_kept_before_runner_assignment(self):
        for state, run_number in (("pending", 2), ("queued", 4)):
            with self.subTest(state=state, run_number=run_number):
                gate = job(2, name="gate")  # No runner, timestamps or started steps.
                code, report = self.invoke(snapshot(run(1), run(2, status=state, run_number=run_number), run(3),
                                                    jobs={1: [], 2: [gate], 3: []}))
                self.assertEqual(code, 0)
                self.assertEqual(report["newest_eligible_run_id"], 3)
                self.assertEqual(report["runs"][0]["disposition"], "superseded")
                self.assertEqual(report["runs"][1]["disposition"], "keep_existing_jobs")

    def test_github_ref_qualified_paths_and_optional_job_attempt(self):
        queued_job = job(1)
        del queued_job["run_attempt"]
        evidence = snapshot(run(1, path=f"{PATH}@main"), run(2, path=f"{PATH}@main"),
                            run(3), run(4, path=f"{PATH}@other"), jobs={1: [queued_job], 2: [], 3: []})
        code, report = self.invoke(evidence)
        self.assertEqual(code, 0)
        self.assertEqual(report["newest_eligible_run_id"], 3)
        self.assertEqual([row["disposition"] for row in report["runs"]],
                         ["keep_existing_jobs", "superseded", "keep_newest_eligible", "keep_outside_selection"])
        self.assert_indeterminate(snapshot(run(1, path=".github/workflows/other.yml@main"), jobs={1: []}))
        evidence["jobs"]["runs/1/attempts/2/jobs"] = evidence["jobs"].pop("runs/1/attempts/1/jobs")
        self.assert_indeterminate(evidence)

    def test_same_run_or_rerun_cannot_supersede_itself(self):
        for row in (run(1), run(1, run_attempt=2)):
            with self.subTest(attempt=row["run_attempt"]):
                code, report = self.invoke(snapshot(row, jobs={1: []}))
                self.assertEqual(code, 0)
                expected = row["run_attempt"] == 1
                self.assertEqual(report["newest_eligible_run_id"], 1 if expected else None)
                self.assertEqual(report["runs"][0]["disposition"],
                                 "keep_newest_eligible" if expected else "keep_outside_selection")
        self.assert_indeterminate(snapshot(run(1), run(1, status="queued", run_attempt=2), jobs={1: []}))

    def test_complete_run_and_job_pages_are_required(self):
        # Only the last row of the second jobs page has a started job.
        job_rows = [job(1, jid) for jid in range(1, 101)] + [job(1, 101, status="in_progress")]
        evidence = snapshot(run(1), *[run(rid, event="workflow_dispatch") for rid in range(2, 101)],
                            run(101), jobs={1: job_rows, 101: []})
        code, report = self.invoke(evidence)
        self.assertEqual(code, 0)
        self.assertEqual(report["newest_eligible_run_id"], 101)
        self.assertEqual(report["runs"][0]["disposition"], "keep_existing_jobs")
        for inventory in (evidence["runs"]["pending"], evidence["jobs"]["runs/1/attempts/1/jobs"]):
            page = inventory.pop()
            self.assert_indeterminate(evidence)
            inventory.append(page | {"total_count": 102})
            self.assert_indeterminate(evidence)
            inventory[-1] = page

    def test_missing_or_cross_attempt_evidence_returns_no_selection(self):
        self.assert_indeterminate(snapshot(run(1), run(2), jobs={2: []}))
        self.assert_indeterminate(snapshot(run(1), run(2), jobs={1: [job(1, run_attempt=2)], 2: []}))
        self.assert_indeterminate(snapshot(run(1), run(2), jobs={1: [job(2)], 2: []}))
        self.assert_indeterminate(snapshot(run(1), run(2), jobs={1: [job(1, head_sha="b" * 40)], 2: []}))
        self.assert_indeterminate(snapshot(run(1), jobs={1: [job(1, run_url=job(2)["run_url"])]}))
        self.assert_indeterminate(snapshot(run(1), run(2), jobs={1: [job(1), job(1)], 2: []}))
        self.assert_indeterminate(snapshot(run(1), jobs={1: []}), "--pin-run", "bad")
        self.assert_indeterminate('{"runs": {}, "runs": {}}')
        evidence = snapshot(run(1), run(2), jobs={1: [], 2: []})
        evidence["runs"]["pending"][0]["total_count"] = 1000
        self.assert_indeterminate(evidence)
        evidence["runs"]["pending"][0]["total_count"] = 2
        del evidence["runs"]["waiting"]
        self.assert_indeterminate(evidence)


if __name__ == "__main__":
    unittest.main()
