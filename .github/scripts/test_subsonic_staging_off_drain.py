"""Exercise the protected read adapter with synthetic inputs and no live calls."""

import io
import os
import signal
import sys
import time
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

import subsonic_staging_off_drain as adapter
from subsonic_staging_cleanup import CleanupResult
from subsonic_staging_observer import PressureObservation
from test_subsonic_staging_cleanup import PIN, deployment, version

READ_TOKEN = "SUBSONIC_STAGING_WORKERS_READ_TOKEN"
OBSERVER = "PLANETSCALE_OBSERVER_URL_STAGING"
ACCOUNT = "a" * 32
ENV = {
    "CLOUDFLARE_ACCOUNT_ID": ACCOUNT,
    READ_TOKEN: "synthetic-read-token",
    OBSERVER: "synthetic-observer-config",
    "EXPECTED_STAGING_DEPLOYMENT_ID": PIN.deployment_id,
    "EXPECTED_STAGING_VERSION_ID": PIN.version_id,
    "EXPECTED_STAGING_BUILD_SHA": PIN.build_sha,
}


class ReadbackTests(unittest.TestCase):
    def test_only_pinned_provider_reads_bracket_one_observer_sample(self):
        events = []
        base = f"/client/v4/accounts/{ACCOUNT}/workers/scripts/nntune-api-staging"

        def read(path, token):
            self.assertEqual(token, ENV[READ_TOKEN])
            self.assertGreater(signal.getitimer(signal.ITIMER_REAL)[0], 0)
            events.append(path)
            return (
                version()
                if path == base + "/versions/" + PIN.version_id
                else deployment()
            )

        def sample(env, *, deadline_ms):
            self.assertIs(env, os.environ)
            observed = time.monotonic() * 1000
            self.assertLess(observed, deadline_ms)
            self.assertLessEqual(deadline_ms - observed, adapter.BUDGET_SECONDS * 1000)
            events.append("sample")
            return PressureObservation(observed, 0, 0)

        with (
            patch.dict(os.environ, ENV, clear=True),
            patch.object(adapter, "provider_result", read),
            patch.object(adapter, "sample_pressure", sample),
            redirect_stdout(io.StringIO()) as output,
        ):
            self.assertEqual(adapter.main(), 0)
        deployments = base + "/deployments"
        self.assertEqual(
            events,
            [
                deployments,
                base + "/versions/" + PIN.version_id,
                deployments,
                "sample",
                deployments,
            ],
        )
        self.assertEqual(output.getvalue(), "subsonic-staging-off-drain: verified\n")
        self.assertEqual(signal.getitimer(signal.ITIMER_REAL), (0, 0))

    def test_missing_read_credential_or_bad_pin_never_dispatches(self):
        with (
            patch.object(adapter, "provider_result") as provider,
            patch.object(adapter, "sample_pressure") as observer,
        ):
            for key in (READ_TOKEN, OBSERVER, "CLOUDFLARE_ACCOUNT_ID"):
                with self.subTest(key=key):
                    env = ENV | {
                        key: "",
                        "CLOUDFLARE_API_TOKEN": "never-use-deploy-token",
                    }
                    self.assertEqual(
                        adapter.readback(env), "readback-config-unavailable"
                    )
            self.assertEqual(
                adapter.readback(ENV | {"EXPECTED_STAGING_VERSION_ID": "../invalid"}),
                CleanupResult.INVALID_PIN,
            )
        provider.assert_not_called()
        observer.assert_not_called()

    def test_failed_or_interrupted_provider_read_fails_closed(self):
        def interrupted(*_):
            signal.raise_signal(signal.SIGALRM)
            self.fail("alarm did not unwind the provider read")

        previous = signal.getsignal(signal.SIGALRM)
        for effect, category in (
            (ValueError(ENV[READ_TOKEN]), "deployment-unverified"),
            (interrupted, "deadline"),
        ):
            with (
                self.subTest(category=category),
                patch.dict(os.environ, ENV, clear=True),
                patch.object(
                    adapter, "provider_result", side_effect=effect
                ) as provider,
                patch.object(adapter, "sample_pressure") as observer,
                redirect_stdout(io.StringIO()) as output,
            ):
                self.assertEqual(adapter.main(), 1)
                provider.assert_called_once()
                observer.assert_not_called()
                self.assertEqual(
                    output.getvalue(), f"subsonic-staging-off-drain: {category}\n"
                )
                self.assertEqual(signal.getitimer(signal.ITIMER_REAL), (0, 0))
                self.assertIs(signal.getsignal(signal.SIGALRM), previous)

    def test_readback_workflow_binds_only_read_secrets_in_the_protected_step(self):
        path = Path(__file__).parents[1] / "workflows/subsonic-staging-off-drain.yml"
        text = path.read_text()
        for guard in (
            "\non:\n  workflow_dispatch:\n    inputs:\n",
            "\npermissions:\n  contents: read\n",
            "github.repository == 'TogetherWeOwn/nntunev2' &&",
            "github.ref == 'refs/heads/main' && github.ref_protected &&",
            "github.workflow_sha == github.sha && github.run_attempt == 1",
            "environment:\n      name: staging\n      deployment: false",
            "ref: ${{ github.sha }}\n          persist-credentials: false",
        ):
            self.assertIn(guard, text)
        prefix, step = text.split(
            "      - name: Read staging off and drained state (no restore)\n"
        )
        self.assertNotIn("secrets.", prefix)
        self.assertNotIn("upload-artifact", text)
        bindings = (
            ("EXPECTED_STAGING_DEPLOYMENT_ID", "inputs.expected_deployment_id"),
            ("EXPECTED_STAGING_VERSION_ID", "inputs.expected_version_id"),
            ("EXPECTED_STAGING_BUILD_SHA", "inputs.expected_build_sha"),
            ("CLOUDFLARE_ACCOUNT_ID", "secrets.CLOUDFLARE_ACCOUNT_ID"),
            (READ_TOKEN, f"secrets.{READ_TOKEN}"),
            (OBSERVER, f"secrets.{OBSERVER}"),
        )
        expected = "        env:\n" + "".join(
            f"          {name}: ${{{{ {source} }}}}\n" for name, source in bindings
        )
        self.assertEqual(
            step,
            expected
            + "        run: exec timeout -k 2s 32s python3 -I .github/scripts/subsonic_staging_off_drain.py 2>/dev/null\n",
        )
        for other in path.parent.iterdir():
            if other not in {path, path.with_name("deploy.yml")} and other.suffix in {
                ".yml",
                ".yaml",
            }:
                self.assertNotIn(READ_TOKEN, other.read_text(), other.name)

    def test_deploy_readback_is_isolated_and_requires_a_same_job_pin(self):
        text = (Path(__file__).parents[1] / "workflows/deploy.yml").read_text()
        prefix, api = text.split("\n  staging-api:\n", 1)
        deploy, readback = api.split(
            "      - name: Read staging off and drained state after API deploy\n", 1
        )
        step, suffix = readback.split(
            "      - name: Record Subsonic staging readback outcome\n", 1
        )
        for secret in (READ_TOKEN, OBSERVER):
            self.assertNotIn(secret, prefix + deploy + suffix)
        self.assertIn("environment:\n      name: staging\n", deploy)
        self.assertIn("id: deployment\n", deploy)
        self.assertEqual(
            step,
            """        id: subsonic_readback
        if: >-
          ${{ success() && github.event_name == 'push' &&
              github.repository == 'TogetherWeOwn/nntunev2' &&
              github.ref == 'refs/heads/main' && github.ref_protected &&
              github.workflow_sha == github.sha && github.run_attempt == 1 &&
              steps.deploy.outcome == 'success' && steps.deployment.outputs.deployed == 'true' &&
              steps.deployment.outputs.identity_pin != '' }}
        # Evidence only: a readback failure does not change deploy or E2E eligibility.
        continue-on-error: true
        env:
          EXPECTED_STAGING_DEPLOYMENT_ID: ${{ fromJSON(steps.deployment.outputs.identity_pin).deployment_id }}
          EXPECTED_STAGING_VERSION_ID: ${{ fromJSON(steps.deployment.outputs.identity_pin).version_id }}
          EXPECTED_STAGING_BUILD_SHA: ${{ fromJSON(steps.deployment.outputs.identity_pin).build_sha }}
          CLOUDFLARE_ACCOUNT_ID: ${{ secrets.CLOUDFLARE_ACCOUNT_ID }}
          SUBSONIC_STAGING_WORKERS_READ_TOKEN: ${{ secrets.SUBSONIC_STAGING_WORKERS_READ_TOKEN }}
          PLANETSCALE_OBSERVER_URL_STAGING: ${{ secrets.PLANETSCALE_OBSERVER_URL_STAGING }}
        run: exec timeout -k 2s 32s python3 -I .github/scripts/subsonic_staging_off_drain.py 2>/dev/null
""",
        )
        self.assertTrue(
            suffix.startswith(
                "        if: ${{ !cancelled() }}\n        continue-on-error: true\n"
            )
        )
        self.assertIn(
            "READBACK_OUTCOME: ${{ steps.subsonic_readback.outcome }}", suffix
        )


if __name__ == "__main__":
    unittest.main()
