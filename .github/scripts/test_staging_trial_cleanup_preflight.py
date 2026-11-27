from contextlib import chdir
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import tomllib
import unittest
from unittest.mock import patch

import private_staging_evidence as evidence
from staging_api_identity import SCRIPT, STREAM, Unavailable
from staging_stream_config import private_vars, render, trial_preflight
from staging_trial_cleanup_preflight import preflight

SHA = "a" * 40
SCOPE_SHA = "b" * 64
ACCESS_KEY_SHA = "c" * 64
VERSION = "12345678-1234-4234-8234-123456789012"
DISABLED_DEPLOYMENT = "22345678-1234-4234-8234-123456789012"
PINS = {"version_id": VERSION, "deployment_id": DISABLED_DEPLOYMENT}
ENV = {
    "CLEANUP_SCOPE_SHA256": SCOPE_SHA,
    "CLEANUP_CONTAINMENT": json.dumps({
        "session_revoked": True, "cohort_cleared": True, "armed_runs_drained": True,
        "build_sha": SHA, "api": PINS, "stream": PINS,
        "cleanup_scope_sha256": SCOPE_SHA, "cleanup_s3_access_key_sha256": ACCESS_KEY_SHA,
    }),
    "PRIVATE_PLAYBACK_STAGING_ENVIRONMENT": VERSION,
    "PRIVATE_PLAYBACK_STAGING_LIBRARY": VERSION,
    "PRIVATE_PLAYBACK_STAGING_S3_CONFIGURATION": VERSION,
    "PRIVATE_PLAYBACK_STAGING_ROOT_VERSION": VERSION,
    "PRIVATE_PLAYBACK_STAGING_S3_BUCKET": "nntunev2-private-fixtures-staging",
    "PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT": "https://objects.example.test",
    "PRIVATE_PLAYBACK_STAGING_S3_REGION": "fixture-region",
}


def read(worker, path):
    if path == "deployments":
        return {"deployments": [{
            "id": DISABLED_DEPLOYMENT, "strategy": "percentage",
            "versions": [{"version_id": VERSION, "percentage": 100}],
        }]}
    assert path == "versions/" + VERSION
    bindings = [{"name": "BUILD_SHA", "text": SHA, "type": "plain_text"}]
    if worker == STREAM:
        bindings.append({"name": "PRIVATE_PLAYBACK_ENABLED", "text": "false", "type": "plain_text"})
    return {"id": VERSION, "resources": {"bindings": bindings}}


class CleanupContainmentTests(unittest.TestCase):
    def test_rechecks_api_after_stream_version_read(self):
        stream_read = False

        def drift(worker, path):
            nonlocal stream_read
            result = read(worker, path)
            if worker == STREAM and path.startswith("versions/"):
                stream_read = True
            if worker == SCRIPT and path == "deployments" and stream_read:
                result["deployments"][0]["id"] = VERSION
            return result

        with self.assertRaises(Unavailable):
            preflight(ENV, drift)

    def test_validates_local_cleanup_inputs_before_reads(self):
        receipt = json.loads(ENV["CLEANUP_CONTAINMENT"])
        for change in (
            {"CLEANUP_CONTAINMENT": json.dumps(receipt | {"stream": {}})},
            {"PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT": "https://user@objects.example.test"},
        ):
            with self.subTest(change=change), self.assertRaises(Unavailable):
                preflight(ENV | change, lambda *_: self.fail("read before local validation"))

    def test_requires_off_pins_and_provider(self):
        self.assertEqual(preflight(ENV, read)["provider"]["environment"], VERSION)
        for overrides in (
            {"CLEANUP_CONTAINMENT": ""},
            {"PRIVATE_TRIAL_UPLOAD_STAGING_ENABLED": "true"},
            {"PRIVATE_MUSIC_STAGING_JOURNEY": "true"},
            {"PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT": "https://name@objects.example.test"},
        ):
            with self.subTest(overrides=overrides), self.assertRaises(Unavailable):
                preflight(ENV | overrides, read)

    def test_requires_approved_scope_and_cleanup_access_key_before_provider_reads(self):
        approved = json.loads(ENV["CLEANUP_CONTAINMENT"])
        for field, value in (
            ("cleanup_scope_sha256", None),
            ("cleanup_scope_sha256", "d" * 64),
            ("cleanup_s3_access_key_sha256", None),
            ("cleanup_s3_access_key_sha256", "not-a-digest"),
        ):
            with self.subTest(field=field, value=value), self.assertRaises(Unavailable):
                receipt = {**approved, field: value}
                if value is None:
                    del receipt[field]
                preflight(ENV | {"CLEANUP_CONTAINMENT": json.dumps(receipt)},
                          lambda *_: self.fail("read before approval passed"))

        approval = preflight(ENV, read)
        self.assertEqual(approval["scope_sha256"], SCOPE_SHA)
        self.assertEqual(approval["s3_access_key_sha256"], ACCESS_KEY_SHA)

    def test_refuses_armed_upload_or_moved_disable_version(self):
        def armed(worker, path):
            result = read(worker, path)
            if worker == SCRIPT and path.startswith("versions/"):
                result["resources"]["bindings"].append({"name": "PRIVATE_TRIAL_UPLOAD_DB", "type": "hyperdrive"})
            return result

        first = True

        def moved(worker, path):
            nonlocal first
            result = read(worker, path)
            if worker == SCRIPT and path == "deployments":
                if not first:
                    result["deployments"][0]["id"] = VERSION
                first = False
            return result

        for provider in (armed, moved):
            with self.subTest(provider=provider.__name__), self.assertRaises(Unavailable):
                preflight(ENV, provider)


class PrivateEvidenceTests(unittest.TestCase):
    def test_capture_refuses_invalid_receipt_ids(self):
        receipt = {"identity": "matched", "build_sha": SHA, "version_id": VERSION,
                   "deployment_id": "invalid"}
        with patch.dict("os.environ", {"API_IDENTITY": json.dumps(receipt),
                                      "GITHUB_RUN_ID": "1", "GITHUB_RUN_ATTEMPT": "1"}):
            with self.assertRaises(evidence.admission.Refused):
                evidence.capture("api", SHA, Path("."))

    def test_assembly_refuses_non_utf8_xml_before_rewriting(self):
        cases = "".join(f'<testcase classname="private-music" name="{phase}"><properties>'
                        '<property name="candidate-identity" value="{}"/></properties></testcase>'
                        for phase in evidence.admission.IDENTITY_PHASES)
        xml = (f'<!DOCTYPE testsuite><testsuite>{cases}</testsuite>').encode("utf-16")
        with TemporaryDirectory() as tmp:
            report = Path(tmp) / "report.xml"
            report.write_bytes(xml)
            with self.assertRaises(evidence.admission.Refused):
                evidence.attach_observations(report, {})
            self.assertEqual(report.read_bytes(), xml)


class StreamConfigTests(unittest.TestCase):
    def test_preflight_reads_config_from_its_checkout(self):
        with TemporaryDirectory() as tmp, chdir(tmp):
            spoofed = Path("workers/stream/wrangler.toml")
            spoofed.parent.mkdir(parents=True)
            spoofed.write_text("this is invalid TOML")
            self.assertFalse(trial_preflight({}))

    def test_refuses_malformed_journey_arm(self):
        with self.assertRaises(ValueError):
            private_vars({"PRIVATE_MUSIC_STAGING_JOURNEY": "tru"}, {})

    def test_refuses_invalid_compatibility_date(self):
        source = tomllib.loads((evidence.ROOT / "workers/stream/wrangler.toml").read_text())
        with self.assertRaises(ValueError):
            render(source | {"compatibility_date": "2026-02-30"}, {})


if __name__ == "__main__":
    unittest.main()
