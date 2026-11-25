import json
import unittest

from staging_api_identity import SCRIPT, STREAM, Unavailable
from staging_trial_cleanup_preflight import preflight

SHA = "a" * 40
VERSION = "12345678-1234-4234-8234-123456789012"
DISABLED_DEPLOYMENT = "22345678-1234-4234-8234-123456789012"
PINS = {"version_id": VERSION, "deployment_id": DISABLED_DEPLOYMENT}
ENV = {
    "CLEANUP_SCOPE_SHA256": "b" * 64,
    "CLEANUP_CONTAINMENT": json.dumps({
        "session_revoked": True, "cohort_cleared": True, "armed_runs_drained": True,
        "build_sha": SHA, "api": PINS, "stream": PINS,
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
    def test_requires_off_pins_and_provider(self):
        self.assertEqual(preflight(ENV, read)["environment"], VERSION)
        for overrides in (
            {"CLEANUP_CONTAINMENT": ""},
            {"PRIVATE_TRIAL_UPLOAD_STAGING_ENABLED": "true"},
            {"PRIVATE_MUSIC_STAGING_JOURNEY": "true"},
            {"PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT": "https://name@objects.example.test"},
        ):
            with self.subTest(overrides=overrides), self.assertRaises(Unavailable):
                preflight(ENV | overrides, read)

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


if __name__ == "__main__":
    unittest.main()
