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
                self.check(workflow=workflow, job=job, worker=worker, approved_worker=worker)
                with self.assertRaisesRegex(ValueError, "Worker target"):
                    self.check(
                        workflow=workflow, job=job, worker=worker.removesuffix("-staging"),
                        approved_worker=worker,
                    )
        for overrides in (
            {"worker": "nntune-api-staging"},
            {"workflow": alerts},
            {"workflow": ".github/workflows/mb-snapshot-spike.yml", "job": "spike"},
            {"job": "gate"},
        ):
            with self.subTest(**overrides), self.assertRaisesRegex(ValueError, "Worker target"):
                self.check(**overrides)

    def test_rejects_substitution_of_another_valid_lane(self):
        with self.assertRaisesRegex(ValueError, "Worker target"):
            self.check(job="staging-api", worker="nntune-api-staging")

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

    def test_rejects_source_revision_or_operation_drift(self):
        for overrides, message in (
            ({"source_sha": "d" * 40}, "source revision"),
            ({"approved_source_sha": "c" * 7, "source_sha": "c" * 7}, "source revision"),
            ({"approved_source_sha": "Ｃ" * 40, "source_sha": "Ｃ" * 40}, "source revision"),
            ({"operation": "rollback"}, "deployment operation"),
            ({"approved_operation": "delete", "operation": "delete"}, "deployment operation"),
            ({"version": "01234567-abcd-4321-9876-0123456789ab"}, "unexpected version"),
        ):
            with self.subTest(**overrides), self.assertRaisesRegex(ValueError, message):
                self.check(**overrides)

    def test_rollback_requires_the_approved_source_version_and_bytes(self):
        known_good = "01234567-abcd-4321-9876-0123456789ab"
        body = b'{"versions":[{"version_id":"01234567-abcd-4321-9876-0123456789ab","percentage":100}]}'
        rollback = dict(
            approved_operation="rollback", operation="rollback",
            approved_version=known_good, version=known_good,
            approved_source_sha="e" * 40, source_sha="e" * 40,
            approved_body_sha256=hashlib.sha256(body).hexdigest(), body=body,
        )
        self.check(**rollback)
        for overrides, message in (
            ({"source_sha": "c" * 40}, "source revision"),
            ({"version": "01234567-abcd-4321-9876-0123456789ac"}, "rollback version"),
            ({"approved_version": None, "version": None}, "rollback version"),
            ({"approved_version": "previous", "version": "previous"}, "rollback version"),
            ({"body": body.replace(b"100", b"99")}, "reviewed bytes"),
        ):
            with self.subTest(**overrides), self.assertRaisesRegex(ValueError, message):
                self.check(**(rollback | overrides))

    def check(self, **overrides):
        body = b"reviewed payload"
        fields = dict(
            workflow=".github/workflows/deploy.yml", job="staging-web",
            approved_account="a" * 32, account="a" * 32,
            approved_worker="nntune-web-staging", worker="nntune-web-staging",
            approved_source_sha="c" * 40, source_sha="c" * 40,
            approved_operation="deploy", operation="deploy",
            approved_version=None, version=None,
            approved_body_sha256=hashlib.sha256(body).hexdigest(), body=body,
        )
        require_reviewed_submission(**(fields | overrides))


if __name__ == "__main__":
    unittest.main()
