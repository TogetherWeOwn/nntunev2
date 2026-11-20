import hashlib
import unittest

from staging_worker_submission import require_reviewed_submission


class StagingWorkerSubmissionTests(unittest.TestCase):
    def test_only_fixed_lane_target_pairs(self):
        deploy = ".github/workflows/deploy.yml"
        alerts = ".github/workflows/deploy-alerts-staging.yml"
        lanes = [
            (deploy, "staging-web", "nntune-web-staging"),
            (deploy, "staging-api", "nntune-api-staging"),
            (deploy, "staging-mb", "nntune-mb-staging"),
            (deploy, "staging-ingest", "nntune-ingest-staging"),
            (alerts, "deploy", "nntune-alerts-staging"),
        ]
        for workflow, job, worker in lanes:
            with self.subTest(workflow=workflow, job=job):
                self.check(workflow=workflow, job=job, worker=worker)
                with self.assertRaisesRegex(ValueError, "Worker target"):
                    self.check(workflow=workflow, job=job, worker=worker.removesuffix("-staging"))
        for overrides in (
            {"worker": "nntune-api-staging"},
            {"workflow": alerts},
            {"workflow": ".github/workflows/mb-snapshot-spike.yml", "job": "spike"},
            {"job": "gate"},
        ):
            with self.subTest(**overrides), self.assertRaisesRegex(ValueError, "Worker target"):
                self.check(**overrides)

    def test_rejects_account_or_body_drift(self):
        for overrides, message in (
            ({"account": "b" * 32}, "account"),
            ({"approved_account": "", "account": ""}, "account"),
            ({"body": b"reviewed payload\n"}, "reviewed bytes"),
            ({"body": bytearray(b"reviewed payload")}, "reviewed bytes"),
            ({"body": b"", "approved_body_sha256": hashlib.sha256(b"").hexdigest()}, "reviewed bytes"),
            ({"approved_body_sha256": ""}, "reviewed bytes"),
        ):
            with self.subTest(**overrides), self.assertRaisesRegex(ValueError, message):
                self.check(**overrides)

    def check(self, **overrides):
        body = b"reviewed payload"
        fields = dict(
            workflow=".github/workflows/deploy.yml", job="staging-web",
            approved_account="a" * 32, account="a" * 32, worker="nntune-web-staging",
            approved_body_sha256=hashlib.sha256(body).hexdigest(), body=body,
        )
        require_reviewed_submission(**(fields | overrides))


if __name__ == "__main__":
    unittest.main()
