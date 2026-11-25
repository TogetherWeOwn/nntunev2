"""Wire protected private qualification to the trusted admission producer."""

import json
import os
from pathlib import Path
import re
import sys
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
sys.path[:0] = [str(ROOT / "ci"), str(Path(__file__).resolve().parent)]
import staging_admission as admission
import staging_api_identity as identity

LANES = ("web", "api", "stream")
PIN_FIELDS = ("worker", "build_sha", "version_id", "deployment_id")
PIN_LIMIT = 1024
REPORT = ROOT / "e2e/private-staging-results/admission.xml"


def read_bounded(path, limit):
    with path.open("rb") as source:
        body = source.read(limit + 1)
    admission.require(len(body) <= limit, "oversized private staging input")
    return body


def provenance():
    return {"run_id": admission.decimal(os.environ["GITHUB_RUN_ID"]),
            "run_attempt": admission.decimal(os.environ["GITHUB_RUN_ATTEMPT"])}


def provider(token):
    account = os.environ.get("CLOUDFLARE_ACCOUNT_ID", "")
    admission.require(re.fullmatch(r"[0-9a-f]{32}", account) and re.fullmatch(r"[\x21-\x7e]{1,512}", token),
                      "invalid staging provider credentials")
    prefix = f"/client/v4/accounts/{account}/workers/scripts/"
    return lambda worker: identity.provider_result(prefix + worker + "/deployments", token)


def capture(lane, sha, runner):
    worker = f"nntune-{lane}-staging"
    if lane == "web":
        # The trusted Web build bakes GITHUB_SHA. Wrangler identifies the exact
        # version just uploaded, and the journey independently reads that bake.
        (output,) = Path(os.environ["WRANGLER_OUTPUT_FILE_DIRECTORY"]).glob("wrangler-output-*.json")
        version = identity.deployed_version(read_bounded(output, identity.MAX_DEPLOY_OUTPUT), worker)
        read = provider(os.environ.get("CLOUDFLARE_API_TOKEN", ""))
        deployment = identity.current_deployment(read(worker), version)
        receipt = {"worker": worker, "build_sha": sha, "version_id": version, "deployment_id": deployment}
    else:
        body = (os.environ.get("API_IDENTITY", "").encode() if lane == "api" else
                read_bounded(runner / "staging-stream-identity.json", PIN_LIMIT))
        receipt = admission.parse_json(body, PIN_LIMIT)
        admission.require(receipt.get("identity") == "matched" and receipt.get("build_sha") == sha,
                          "missing matched staging deployment receipt")
        if lane == "stream":
            admission.require(receipt.get("worker") == worker and receipt.get("private_playback_enabled") is True,
                              "private Stream mode was not verified")
        # The existing API receipt validator already binds the fixed API Worker.
        receipt["worker"] = worker
    return {**{key: receipt[key] for key in PIN_FIELDS}, **provenance()}


def deployment_pins(sha):
    pins = {f"staging-{lane}": admission.parse_json(os.environ.get(f"{lane.upper()}_PIN", "").encode(), PIN_LIMIT)
            for lane in LANES}
    return pins, admission.private_deployments(pins, sha, provenance())


def observe(pins, read):
    observed = {}
    for lane, pin in pins.items():
        deployment = identity.current_deployment(read(pin["worker"]), pin["version_id"])
        admission.require(deployment == pin["deployment_id"], "staging deployment drifted")
        observed[lane] = {**pin, "deployment_id": deployment}
    return observed


def attach_observations(report, observations):
    xml = read_bounded(report, admission.MAX_XML)
    admission.require(b"<!DOCTYPE" not in xml and b"<!ENTITY" not in xml, "unsafe private staging XML")
    suite = ET.fromstring(xml)
    for phase, deployments in observations.items():
        properties = suite.findall(f"./testcase[@classname='private-music'][@name='{phase}']"
                                   "/properties/property[@name='candidate-identity']")
        admission.require(len(properties) == 1, "missing private staging identity")
        (prop,) = properties
        value = admission.parse_json(prop.attrib["value"].encode(), PIN_LIMIT)
        value["deployments"] = deployments
        prop.set("value", json.dumps(value, separators=(",", ":")))
    report.write_bytes(ET.tostring(suite, encoding="utf-8"))


def main(command):
    sha = os.environ["GITHUB_SHA"]
    admission.require(admission.SHA.fullmatch(sha), "invalid private staging SHA")
    runner = Path(os.environ["RUNNER_TEMP"])
    if command in LANES:
        return capture(command, sha, runner)
    pins, expected = deployment_pins(sha)
    if command == "observe":
        return observe(expected, provider(os.environ.get("SUBSONIC_STAGING_WORKERS_READ_TOKEN", "")))
    if command == "assemble":
        observations = {phase: admission.parse_json(read_bounded(runner / f"{phase}.json", PIN_LIMIT), PIN_LIMIT)
                        for phase in admission.IDENTITY_PHASES}
        admission.require(all(observation == expected for observation in observations.values()),
                          "private staging boundary pins differ")
        attach_observations(REPORT, observations)
        return pins
    raise ValueError


if __name__ == "__main__":
    try:
        print(json.dumps(main(sys.argv[1]), separators=(",", ":")))
    except (IndexError, KeyError, OSError, ValueError, RecursionError, ET.ParseError, admission.Refused, identity.Unavailable):
        sys.exit("Private staging deployment evidence unavailable or mismatched.")
