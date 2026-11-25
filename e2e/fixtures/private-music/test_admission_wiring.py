"""Exercise Playwright, review evidence and the receipt step offline."""

from hashlib import sha256
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
from unittest.mock import call, patch
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[3]
HELPER = ROOT / ".github/scripts/private_staging_evidence.py"
SPEC = importlib.util.spec_from_file_location("private_staging_evidence", HELPER)
evidence = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(evidence)
admission = evidence.admission
VERSION = "11111111-1111-4111-8111-111111111111"
DEPLOYMENT = "22222222-2222-4222-8222-222222222222"
REDEPLOYMENT = "33333333-3333-4333-8333-333333333333"
PRIVATE_DETAIL = "https://private.example.invalid/listen?token=disposable-canary&account=account@example.invalid"
PRIVATE_MARKERS = (b"private.example.invalid", b"disposable-canary", b"account@example.invalid")
AUDIO_DIR = "fixtures/synthetic-flac/private-staging"
TRACK_FILE = f"{AUDIO_DIR}/tone-a.flac"
PUBLISHED_TRACK = "00000000-0000-4000-8000-000000000001"
COMPANION_TRACK = "00000000-0000-4000-8000-000000000002"


def write(root, name, contents):
    path = root / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(contents)


def setup(root, env):
    for name in ("e2e/package.json", "e2e/playwright.private-staging.config.ts", "e2e/helpers/private-staging-admission-reporter.ts",
                 "e2e/helpers/private-staging-fixture.ts", "e2e/private-staging/track-evidence.ts",
                 TRACK_FILE, f"{AUDIO_DIR}/tone-b.flac", f"{AUDIO_DIR}/manifest.json",
                 "ci/staging_admission.py", ".github/scripts/private_staging_evidence.py",
                 ".github/scripts/staging_api_identity.py"):
        write(root, name, (ROOT / name).read_bytes())
    (root / "e2e/node_modules").symlink_to(ROOT / "e2e/node_modules", target_is_directory=True)
    write(root, "migrations/manifest.toml", b"# disposable manifest\n")
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
    required_criteria = sorted(admission.PRIVATE_CRITERIA | {"invite-acceptance"})
    criteria = [{"id": key, "applicable": True,
                 "evidence": [{"classname": "private-music", "name": key, "outcome": "passed"}]}
                for key in required_criteria]
    contract = json.dumps({"schema": 1, "evaluator": "junit-map-v1", "journey": "private-music",
                           "spec_revision": "fixture", "criteria": criteria}).encode()
    write(root, admission.CONTRACT, contract)
    write(root, admission.POLICY, json.dumps({"schema": 1, "evaluator": "junit-map-v1", "approvals": [{
        "sha256": sha256(contract).hexdigest(), "spec_revision": "fixture",
        "product_approval": "fixture", "qa_approval": "fixture",
    }]}).encode())
    write(root, "e2e/private-staging/fixture.spec.ts", f"""
import {{ test }} from '@playwright/test';
import {{ freshTrialPublication, privateStagingAudio }} from '../helpers/private-staging-fixture.js';
import {{ privateRunIdentity, privateTrackObservation }} from './track-evidence.js';
const detail = {json.dumps(PRIVATE_DETAIL)};
test(detail, async ({{}}, info) => {{
  console.log(detail);
  console.error(detail);
  if (process.env.FIXTURE_OUTCOME === 'expected') test.fail();
  if (['failure', 'expected'].includes(process.env.FIXTURE_OUTCOME ?? '')) throw new Error(detail);
  if (process.env.FIXTURE_OUTCOME === 'retry' && info.retry === 0) throw new Error(detail);
  info.annotations.push({{ type: 'ignored', description: detail }});
  await info.attach(detail, {{ body: detail, contentType: 'text/plain' }});
  const upload = privateStagingAudio().find(({{ file }}) => file === 'tone-a.flac')!;
  const publicationReceipt = {{ trackId: '{PUBLISHED_TRACK}', sha256: upload.sha256 }};
  const publication = freshTrialPublication(JSON.stringify(publicationReceipt), upload.sha256, ['{COMPANION_TRACK}']);
  const libraryRow = {{ trackId: '{PUBLISHED_TRACK}' }};
  const playReceipt = {{ trackId: process.env.FIXTURE_OUTCOME === 'other-track' ? '{COMPANION_TRACK}' : '{PUBLISHED_TRACK}' }};
  const run = privateRunIdentity(process.env.GITHUB_SHA!, process.env.GITHUB_RUN_ID, process.env.GITHUB_RUN_ATTEMPT);
  const observe = (type: string, observedTrackId: string) => {{
    const proof = privateTrackObservation(run, publication, observedTrackId);
    info.annotations.push({{ type, description: JSON.stringify({{ ...proof, ignored: detail }}) }});
  }};
  observe('private-publication', publication.trackId);
  observe('private-library', libraryRow.trackId);
  observe('private-play', playReceipt.trackId);
  for (const type of ['private-anonymous-refusal', 'private-unlisted-refusal', 'private-cutoff-refusal'])
    observe(type, publication.trackId);
  const phases = {json.dumps(admission.IDENTITY_PHASES)};
  for (const [index, phase] of phases.entries()) {{
    await test.step(phase, async () => info.annotations.push({{
      type: 'candidate-identity', description: JSON.stringify({{
        phase, web: '{sha}', api: '{sha}', expected: '{sha}',
        observed_at: `2025-01-01T00:00:0${{index}}.000Z`, ignored: detail,
      }}),
    }}));
    if (index === 0) for (const name of {json.dumps(required_criteria)}) {{
      const title = name === 'invite-acceptance' ? 'invite redemption and Library arrival' : name;
      if (name === 'invite-acceptance' && process.env.FIXTURE_OUTCOME === 'skip') await test.step.skip(title, async () => {{}});
      else await test.step(title, async () => {{}});
    }}
  }}
}});
""".encode())


def protected_steps():
    workflow = (ROOT / admission.WORKFLOW).read_text()

    def job(name):
        section = workflow.split(f"\n  {name}:\n", 1)[1]
        return re.split(r"\n  [\w-]+:\n", section, maxsplit=1)[0]

    qualify = job("post-deploy-private-music")
    qualify_settings, _ = qualify.split("\n    steps:\n", 1)
    assert "needs: [staging-web, staging-api, staging-stream]" in qualify_settings
    for lane in evidence.LANES:
        settings, steps = job(f"staging-{lane}").split("\n    steps:\n", 1)
        assert "      private_pin: ${{ steps.private-pin.outputs.pin }}\n" in settings + "\n"
        assert f"      {lane.upper()}_PIN: ${{{{ needs.staging-{lane}.outputs.private_pin }}}}" in qualify_settings
        bind = steps.split("        id: private-pin\n", 1)[1].split("\n      - ", 1)[0]
        assert "if: vars.PRIVATE_MUSIC_STAGING_JOURNEY == 'true'" in bind
        assert "printf 'pin=%s\\n' \"$pin\" >> \"$GITHUB_OUTPUT\"" in bind
        if lane == "web":
            assert "PRIVATE_MUSIC_STAGING_JOURNEY: ${{ vars.PRIVATE_MUSIC_STAGING_JOURNEY == 'true' && 'true' || 'false' }}" in steps
            assert 'private_staging_evidence.py web > "$RUNNER_TEMP/staging-web-pin.json"' in steps
            assert 'pin=$(cat "$RUNNER_TEMP/staging-web-pin.json")' in bind
        else:
            assert f"private_staging_evidence.py {lane})" in bind
    record, upload = qualify.split("\n      - name: Upload candidate admission receipt\n")
    assert upload.count("          path:") == 1 and "          path: staging-admission.json\n" in upload
    assert "retention-days: 7" in upload and "${{ github.run_attempt }}-private-music" in upload
    review = qualify.split("\n      - name: Summarize private music contract enrollment review evidence\n", 1)[1]
    review, _ = review.split("\n      - name: Observe closing private staging deployments\n", 1)
    assert qualify.index("      - name: Qualify private listening\n") < qualify.index(
        "      - name: Summarize private music contract enrollment review evidence\n")
    assert review == ("        continue-on-error: true\n"
                      "        run: python3 -I .github/scripts/private_staging_evidence.py review")
    return textwrap.dedent(record.rsplit("\n        run: |\n", 1)[1]), review.split("run: ", 1)[1]


def provider_response(deployment, version=VERSION):
    return {"deployments": [{"id": deployment, "strategy": "percentage",
                             "versions": [{"version_id": version, "percentage": 100}]}]}


def require_refusal(action, reason):
    try:
        action()
    except (admission.Refused, evidence.identity.Unavailable):
        return
    raise AssertionError(reason)


def capture_pins(root, env):
    readback = {"build_sha": env["GITHUB_SHA"], "version_id": VERSION, "deployment_id": DEPLOYMENT}
    api = {"identity": "matched", **readback}
    stream = {**api, "worker": "nntune-stream-staging", "private_playback_enabled": True}
    stream_file = "staging-stream-identity.json"
    write(root, stream_file, json.dumps(stream).encode())
    wrangler = {"type": "deploy", "version": 1, "worker_name": "nntune-web-staging",
                "wrangler_environment": "staging", "version_id": VERSION}
    write(root, "wrangler/wrangler-output-fixture.json", (json.dumps(wrangler) + "\n").encode())
    capture_env = env | {"WRANGLER_OUTPUT_FILE_DIRECTORY": str(root / "wrangler"), "API_IDENTITY": json.dumps(api)}

    with patch.dict(os.environ, capture_env, clear=True), patch.object(evidence, "provider") as provider:
        reader = provider.return_value
        reader.return_value = provider_response(DEPLOYMENT)
        pins = {lane: evidence.main(lane) for lane in evidence.LANES}
        reader.assert_called_once_with("nntune-web-staging")
        assert pins == {lane: {**readback, "worker": f"nntune-{lane}-staging",
                               "run_id": env["GITHUB_RUN_ID"], "run_attempt": env["GITHUB_RUN_ATTEMPT"]}
                        for lane in evidence.LANES}

        reader.return_value = provider_response(DEPLOYMENT, version=DEPLOYMENT)
        require_refusal(lambda: evidence.main("web"), "Web captured a different serving version")
        for invalid_api in ({"identity": "unavailable"}, {"build_sha": "b" * 40}):
            os.environ["API_IDENTITY"] = json.dumps(api | invalid_api)
            require_refusal(lambda: evidence.main("api"), "API captured an unavailable or wrong-build identity")
        for invalid_stream in ({"private_playback_enabled": False}, {"worker": "nntune-api-staging"}):
            write(root, stream_file, json.dumps(stream | invalid_stream).encode())
            require_refusal(lambda: evidence.main("stream"), "Stream captured an unarmed or wrong-Worker identity")
    return {f"{lane.upper()}_PIN": json.dumps(pin) for lane, pin in pins.items()}


def check_review(report, summary, run, command, env):
    raw = report.read_bytes()
    sha, run_id, attempt = (env[key] for key in ("GITHUB_SHA", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT"))
    draft = admission.parse_json((ROOT / "ci/private-music-journey-contract.draft.json").read_bytes())
    assert draft["journey"] == admission.PRIVATE
    names = list(admission.IDENTITY_PHASES)
    excluded = []
    for criterion in draft["criteria"]:
        name = criterion["id"]
        if criterion["applicable"] is False:
            assert "evidence" not in criterion, "excluded criterion has an observed case selector"
            excluded.append(name)
            continue
        assert criterion["applicable"] is True
        assert criterion["evidence"] == [{"classname": admission.PRIVATE, "name": name, "outcome": "passed"}], (
            "each applicable criterion must select its own private-music case exactly once"
        )
        names.append(name)
    assert excluded == ["paid-signup"] and "paid-signup" not in names, "paid signup must stay excluded"
    assert len(names) == len(set(names)), "each projected case must map exactly once"
    expected = [
        "## Private music contract enrollment review evidence", "",
        f"Workflow run `{run_id}`, attempt `{attempt}`, build `{sha}`.", "",
        "| Case (class `private-music`) | Observed first-attempt outcome |",
        "| --- | --- |", *(f"| `{name}` | passed |" for name in sorted(names)),
    ]
    result = run(command)
    assert result.returncode == 0 and not (result.stdout or result.stderr)
    assert summary.read_text().splitlines() == expected, "review summary diverges from the draft or synthetic run"
    assert report.read_bytes() == raw
    summary.unlink()

    root = ET.fromstring(raw)
    track = root.find("./testcase[@name='upload-to-library']/properties/property")
    track.set("value", json.dumps(json.loads(track.get("value")) | {"extra": PRIVATE_DETAIL}))
    unexpected_property = ET.tostring(root)
    root = ET.fromstring(raw)
    root.append(root[0])
    duplicate_case = ET.tostring(root)
    root = ET.fromstring(raw)
    root.remove(root[0])
    missing_case = ET.tostring(root)
    root = ET.fromstring(raw)
    ET.SubElement(root, "testcase", classname=admission.PRIVATE, name="paid-signup", retry="0")
    unexpected_paid_signup = ET.tostring(root)
    root = ET.fromstring(raw)
    props = root.find("./testcase[@name='upload-to-library']/properties")
    props[1].set("name", props[0].get("name"))
    duplicate_property = ET.tostring(root)
    for invalid in (
        b"", raw[:-16], raw + b" " * (16 * 1024), b"\xff" + raw,
        b"<!DOCTYPE testsuite>" + raw, b"<?private marker?>" + raw,
        raw.replace(b"<testsuite>", b'<testsuite xmlns:unused="urn:private">', 1),
        missing_case, duplicate_case, unexpected_paid_signup, duplicate_property, unexpected_property,
        raw.replace(b'classname="private-music"', b'classname="other"', 1),
        raw.replace(b'name="play"', b'name="disposable-canary"', 1),
        raw.replace(b'retry="0"', b'retry="1"', 1),
        raw.replace(b"</testcase>", b"<failure>disposable-canary</failure></testcase>", 1),
        raw.replace(b"</testcase>", b"<rerunFailure/></testcase>", 1),
        raw.replace(b"</testcase>", b"<skipped/></testcase>", 1),
        raw.replace(b"&quot;run_attempt&quot;:&quot;2&quot;", b"&quot;run_attempt&quot;:&quot;1&quot;", 1),
        raw.replace(sha.encode(), b"0" * 40, 1),
    ):
        assert invalid != raw
        report.write_bytes(invalid)
        result = run(command)
        assert result.returncode != 0 and not result.stdout and not summary.exists()
        assert result.stderr == b"Private music contract enrollment review evidence unavailable.\n"
    report.write_bytes(raw)
    result = run(command, {"GITHUB_RUN_ID": PRIVATE_DETAIL})
    assert result.returncode != 0 and not result.stdout and not summary.exists()


def run_fixture(root):
    env = {"PATH": os.environ["PATH"], "CI": "1", "RUNNER_TEMP": str(root),
           "GITHUB_RUN_ID": "42", "GITHUB_RUN_ATTEMPT": "2",
           "GITHUB_REPOSITORY": "TogetherWeOwn/nntunev2", "GITHUB_EVENT_NAME": "push",
           "GITHUB_REF": "refs/heads/main", "API_DEPLOYED": "true", "API_DEPLOY_OUTCOME": "deployed"}
    setup(root, env)
    record, review = protected_steps()
    command = ["bash", "-c", record]
    review_command = ["bash", "-c", review]
    summary = root / "review-summary.md"
    env["GITHUB_STEP_SUMMARY"] = str(summary)
    env.update(capture_pins(root, env))

    def run(args, env_overrides=None):
        process = subprocess.run(args, cwd=root, env=env | (env_overrides or {}), capture_output=True, timeout=15)
        assert all(marker not in process.stdout + process.stderr for marker in PRIVATE_MARKERS)
        return process

    def playwright(kind):
        args = [sys.argv[1], "e2e/node_modules/playwright/cli.js", "test", "--config=e2e/playwright.private-staging.config.ts"]
        if kind == "retry":
            args.append("--retries=1")
        return run(args, {"FIXTURE_OUTCOME": kind})

    report = root / "e2e/private-staging-results/admission.xml"
    for refusal in ("failure", "skip", "expected", "retry", "other-track"):
        assert playwright(refusal).returncode != 0 and not report.exists()
    result = playwright("pass")
    assert result.returncode == 0, (result.stdout, result.stderr)
    cases = ET.parse(report)
    assert {case.attrib["retry"] for case in cases.iter("testcase")} == {"0"}
    expected_track = {"track_id": PUBLISHED_TRACK, "sha256": sha256((ROOT / TRACK_FILE).read_bytes()).hexdigest(),
                      "build_sha": env["GITHUB_SHA"], "run_id": env["GITHUB_RUN_ID"],
                      "run_attempt": env["GITHUB_RUN_ATTEMPT"]}
    for case, name in (("upload-to-library", "private-publication"), ("upload-to-library", "private-library"),
                       ("play", "private-play")):
        properties = cases.findall(f"./testcase[@name='{case}']/properties/property[@name='{name}']")
        assert [json.loads(prop.attrib["value"]) for prop in properties] == [expected_track], name
    assert all(marker not in report.read_bytes() for marker in PRIVATE_MARKERS)
    empty_policy = b'{"schema":1,"evaluator":"junit-map-v1","approvals":[]}'
    policy = root / admission.POLICY
    reviewed_policy = policy.read_bytes()
    policy.write_bytes(empty_policy)
    check_review(report, summary, run, review_command, env)
    policy.write_bytes(reviewed_policy)

    receipt = root / "staging-admission.json"
    closing = root / "build-identity-final.json"
    with patch.dict(os.environ, env, clear=True), patch.object(evidence, "provider") as provider:
        reader = provider.return_value
        reader.return_value = provider_response(DEPLOYMENT)
        for phase in admission.IDENTITY_PHASES:
            write(root, f"{phase}.json", json.dumps(evidence.main("observe")).encode())
        assert reader.call_count == 6
        matching_closing = closing.read_bytes()
        reader.return_value = provider_response(VERSION)
        require_refusal(lambda: evidence.main("observe"), "drifted provider deployment accepted")

        # Observe a redeployment of the same version under independently valid pins for this attempt.
        api_pin = json.loads(env["API_PIN"]) | {"deployment_id": REDEPLOYMENT}
        reader.side_effect = lambda worker: provider_response(REDEPLOYMENT if worker == api_pin["worker"] else DEPLOYMENT)
        with patch.dict(os.environ, {"API_PIN": json.dumps(api_pin)}):
            drifted_closing = evidence.main("observe")

        # The closing observation redirects its output before reading the serving deployments.
        reader.reset_mock()
        reader.side_effect = [provider_response(DEPLOYMENT), {"deployments": []}]
        with closing.open("w") as output:
            require_refusal(lambda: json.dump(evidence.main("observe"), output),
                            "closing observation accepted a Worker without a serving deployment")
        assert reader.call_args_list == [call("nntune-web-staging"), call("nntune-api-staging")]

    assert closing.read_bytes() == b"", "failed observation wrote partial deployment evidence"
    original_report = report.read_bytes()
    result = run(command)
    assert result.returncode != 0 and not receipt.exists(), "incomplete readback qualified the candidate"
    assert result.stderr == b"Private staging deployment evidence unavailable or mismatched.\n"
    assert report.read_bytes() == original_report, "incomplete readback changed the report"

    closing.write_text(json.dumps(drifted_closing))
    result = run(command)
    assert result.returncode != 0, "receipt step accepted a changed serving deployment"
    assert not receipt.exists(), "receipt step wrote a receipt after serving deployment drift"
    closing.write_bytes(matching_closing)

    mismatched = json.loads(env["API_PIN"]) | {"run_attempt": "1"}
    for bad_pins in ({"WEB_PIN": ""}, {"API_PIN": json.dumps(mismatched)}):
        assert run(command, bad_pins).returncode != 0 and not receipt.exists()
    result = run(command)
    assert result.returncode == 0, (result.stdout, result.stderr)
    saved = receipt.read_bytes()
    assert all(marker not in saved for marker in PRIVATE_MARKERS)
    recorded = json.loads(saved)
    assert recorded["result"] == "satisfied" and recorded["private_track"] == expected_track
    policy.write_bytes(empty_policy)
    assert run(command).returncode != 0 and json.loads(receipt.read_bytes())["result"] == "incomplete"


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="private-staging-admission-") as temp:
        run_fixture(Path(temp))
