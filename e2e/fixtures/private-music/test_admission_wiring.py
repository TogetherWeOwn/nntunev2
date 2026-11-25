"""Exercise Playwright and the actual receipt step without browser or network access."""

from hashlib import sha256
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap
from unittest.mock import patch
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[3]
HELPER = ROOT / ".github/scripts/private_staging_evidence.py"
SPEC = importlib.util.spec_from_file_location("private_staging_evidence", HELPER)
evidence = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(evidence)
admission = evidence.admission
VERSION = "11111111-1111-4111-8111-111111111111"
DEPLOYMENT = "22222222-2222-4222-8222-222222222222"
PRIVATE_DETAIL = "https://private.example.invalid/listen?token=disposable-canary&account=account@example.invalid"
PRIVATE_MARKERS = (b"private.example.invalid", b"disposable-canary", b"account@example.invalid")
TRACK_FILE = "fixtures/synthetic-flac/private-staging/tone-a.flac"
TRACK = b"disposable synthetic track"


def write(root, name, contents):
    path = root / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(contents)


def setup(root, env):
    for name in ("e2e/package.json", "e2e/playwright.private-staging.config.ts", "e2e/private-staging-admission-reporter.ts",
                 "ci/staging_admission.py", ".github/scripts/private_staging_evidence.py",
                 ".github/scripts/staging_api_identity.py"):
        write(root, name, (ROOT / name).read_bytes())
    (root / "e2e/node_modules").symlink_to(ROOT / "e2e/node_modules", target_is_directory=True)
    write(root, "migrations/manifest.toml", b"# disposable manifest\n")
    write(root, TRACK_FILE, TRACK)
    # The production CLI binds the audio to the candidate's Git object.
    def git(*args):
        return subprocess.check_output(["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                                        "-c", "core.hooksPath=/dev/null", *args], cwd=root,
                                       env={"PATH": env["PATH"], "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1"},
                                       text=True, timeout=10).strip()
    git("init", "--quiet", "--template=")
    git("add", "--", TRACK_FILE)
    git("commit", "--quiet", "-m", "Synthetic fixture")
    sha = git("rev-parse", "HEAD")
    env.update(GITHUB_SHA=sha, GITHUB_WORKFLOW_SHA=sha, API_SOURCE_SHA=sha)
    criteria = [{"id": key, "applicable": True,
                 "evidence": [{"classname": "private-music", "name": key, "outcome": "passed"}]}
                for key in sorted(admission.PRIVATE_CRITERIA)]
    contract = json.dumps({"schema": 1, "evaluator": "junit-map-v1", "journey": "private-music",
                           "spec_revision": "fixture", "criteria": criteria}).encode()
    write(root, admission.CONTRACT, contract)
    write(root, admission.POLICY, json.dumps({"schema": 1, "evaluator": "junit-map-v1", "approvals": [{
        "sha256": sha256(contract).hexdigest(), "spec_revision": "fixture",
        "product_approval": "fixture", "qa_approval": "fixture",
    }]}).encode())
    proof = {"track_id": VERSION, "sha256": sha256(TRACK).hexdigest(), "build_sha": sha,
             "run_id": env["GITHUB_RUN_ID"], "run_attempt": env["GITHUB_RUN_ATTEMPT"], "ignored": PRIVATE_DETAIL}
    names = ["private-publication", "private-library", "private-play", "private-anonymous-refusal",
             "private-unlisted-refusal", "private-cutoff-refusal"]
    write(root, "e2e/private-staging/fixture.spec.ts", f"""
import {{ test }} from '@playwright/test';
const detail = {json.dumps(PRIVATE_DETAIL)};
test(detail, async ({{}}, info) => {{
  console.log(detail);
  console.error(detail);
  if (process.env.FIXTURE_OUTCOME === 'expected') test.fail();
  if (['failure', 'expected'].includes(process.env.FIXTURE_OUTCOME ?? '')) throw new Error(detail);
  if (process.env.FIXTURE_OUTCOME === 'retry' && info.retry === 0) throw new Error(detail);
  info.annotations.push({{ type: 'ignored', description: detail }});
  await info.attach(detail, {{ body: detail, contentType: 'text/plain' }});
  const phases = {json.dumps(admission.IDENTITY_PHASES)};
  for (const [index, phase] of phases.entries()) {{
    await test.step(phase, async () => info.annotations.push({{
      type: 'candidate-identity', description: JSON.stringify({{
        phase, web: '{sha}', api: '{sha}', expected: '{sha}',
        observed_at: `2025-01-01T00:00:0${{index}}.000Z`, ignored: detail,
      }}),
    }}));
    if (index === 0) for (const name of {json.dumps(sorted(admission.PRIVATE_CRITERIA))}) {{
      if (name === 'seek' && process.env.FIXTURE_OUTCOME === 'skip') await test.step.skip(name, async () => {{}});
      else await test.step(name, async () => {{}});
    }}
  }}
  for (const type of {json.dumps(names)})
    info.annotations.push({{ type, description: JSON.stringify({json.dumps(proof)}) }});
}});
""".encode())


def receipt_step():
    workflow = (ROOT / admission.WORKFLOW).read_text().split("\n  post-deploy-private-music:\n", 1)[1]
    record, upload = workflow.split("\n      - name: Upload candidate admission receipt\n")
    assert upload.count("          path:") == 1 and "          path: staging-admission.json\n" in upload
    assert "retention-days: 7" in upload and "${{ github.run_attempt }}-private-music" in upload
    return textwrap.dedent(record.rsplit("\n        run: |\n", 1)[1])


def run_fixture(root):
    env = {"PATH": os.environ["PATH"], "CI": "1", "RUNNER_TEMP": str(root),
           "GITHUB_RUN_ID": "42", "GITHUB_RUN_ATTEMPT": "2",
           "GITHUB_REPOSITORY": "TogetherWeOwn/nntunev2", "GITHUB_EVENT_NAME": "push",
           "GITHUB_REF": "refs/heads/main", "API_DEPLOYED": "true", "API_DEPLOY_OUTCOME": "deployed"}
    setup(root, env)
    for lane in evidence.LANES:
        env[f"{lane.upper()}_PIN"] = json.dumps({"worker": f"nntune-{lane}-staging", "build_sha": env["GITHUB_SHA"],
            "version_id": VERSION, "deployment_id": DEPLOYMENT, "run_id": "42", "run_attempt": "2"})

    def run(args, env_overrides=None):
        process = subprocess.run(args, cwd=root, env=env | (env_overrides or {}), capture_output=True, timeout=15)
        assert all(marker not in process.stdout + process.stderr for marker in PRIVATE_MARKERS)
        return process

    def playwright(kind):
        args = [sys.argv[1], "e2e/node_modules/playwright/cli.js", "test", "--config=e2e/playwright.private-staging.config.ts"]
        if kind == "retry":
            args.append("--retries=1")
        return run(args, {"FIXTURE_OUTCOME": kind})

    def provider_response(deployment):
        return {"deployments": [{"id": deployment, "strategy": "percentage",
                                 "versions": [{"version_id": VERSION, "percentage": 100}]}]}

    report = root / "e2e/private-staging-results/admission.xml"
    for refusal in ("failure", "skip", "expected", "retry"):
        assert playwright(refusal).returncode != 0 and not report.exists()
    result = playwright("pass")
    assert result.returncode == 0, (result.stdout, result.stderr)
    assert {case.attrib["retry"] for case in ET.parse(report).iter("testcase")} == {"0"}
    assert all(marker not in report.read_bytes() for marker in PRIVATE_MARKERS)

    with patch.dict(os.environ, env, clear=True), patch.object(evidence, "provider") as provider:
        provider.return_value.side_effect = lambda _: provider_response(DEPLOYMENT)
        for phase in admission.IDENTITY_PHASES:
            write(root, f"{phase}.json", json.dumps(evidence.main("observe")).encode())
        assert provider.return_value.call_count == 6
        provider.return_value.side_effect = lambda _: provider_response(VERSION)
        try:
            evidence.main("observe")
        except admission.Refused:
            pass
        else:
            raise AssertionError("drifted provider deployment accepted")

    command = ["bash", "-c", receipt_step()]
    receipt = root / "staging-admission.json"
    mismatched = json.loads(env["API_PIN"]) | {"run_attempt": "1"}
    for bad_pins in ({"WEB_PIN": ""}, {"API_PIN": json.dumps(mismatched)}):
        assert run(command, bad_pins).returncode != 0 and not receipt.exists()
    result = run(command)
    assert result.returncode == 0, (result.stdout, result.stderr)
    saved = receipt.read_bytes()
    assert all(marker not in saved for marker in PRIVATE_MARKERS)
    assert json.loads(saved)["result"] == "satisfied"
    write(root, admission.POLICY, b'{"schema":1,"evaluator":"junit-map-v1","approvals":[]}')
    assert run(command).returncode != 0 and json.loads(receipt.read_bytes())["result"] == "incomplete"


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="private-staging-admission-") as temp:
        run_fixture(Path(temp))
