"""Wire protected private qualification to the trusted admission producer."""

import io
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
REVIEW_REPORT_LIMIT = 16 * 1024
REPORT = ROOT / "e2e/private-staging-results/admission.xml"


def read_bounded(path, limit):
    with path.open("rb") as source:
        body = source.read(limit + 1)
    admission.require(len(body) <= limit, "oversized private staging input")
    return body


def provenance():
    return {"run_id": admission.decimal(os.environ["GITHUB_RUN_ID"]),
            "run_attempt": admission.decimal(os.environ["GITHUB_RUN_ATTEMPT"])}


def require_review(condition):
    admission.require(condition, "invalid private staging review report")


def review_xml_shape(node, tag, fields):
    require_review(node.tag == tag and node.attrib.keys() == set(fields)
                   and not (node.text or "").strip(" \t\r\n") and not (node.tail or "").strip(" \t\r\n"))


def review_cases(xml, sha, attempt):
    # The reporter emits only ASCII, with no declarations, comments, namespaces
    # or diagnostic nodes. Parse that profile before publishing.
    require_review(0 < len(xml) <= REVIEW_REPORT_LIMIT and b"<!" not in xml and b"<?" not in xml)
    parser = ET.iterparse(io.StringIO(xml.decode("ascii")), events=("start-ns", "end"))
    for event, _ in parser:
        require_review(event == "end")
    suite = parser.root
    review_xml_shape(suite, "testsuite", ())
    expected = {name: set() for name in admission.PRIVATE_CRITERIA | {"invite-acceptance"}}
    for phase in admission.IDENTITY_PHASES:
        expected[phase] = {"candidate-identity"}
    for proof, name in admission.PRIVATE_TRACK_PROOFS.items():
        expected[name].add(proof)

    observed = set()
    for case in suite:
        review_xml_shape(case, "testcase", ("classname", "name", "retry"))
        name = case.attrib["name"]
        require_review(case.attrib["classname"] == admission.PRIVATE and case.attrib["retry"] == "0"
                       and name in expected and name not in observed)
        proofs = expected[name].copy()
        require_review(len(case) == bool(proofs))
        if proofs:
            (properties,) = case
            review_xml_shape(properties, "properties", ())
            require_review(len(properties) == len(proofs))
            for prop in properties:
                review_xml_shape(prop, "property", ("name", "value"))
                require_review(not len(prop) and prop.attrib["name"] in proofs)
                proofs.remove(prop.attrib["name"])
                value = admission.parse_json(prop.attrib["value"].encode(), PIN_LIMIT)
                if name in admission.IDENTITY_PHASES:
                    fields = {"phase": name, "web": sha, "api": sha, "expected": sha}
                    timestamp = value.get("observed_at")
                    require_review(value.keys() == fields.keys() | {"observed_at"}
                                   and isinstance(timestamp, str)
                                   and re.fullmatch(r"[0-9]{4}(?:-[0-9]{2}){2}T[0-9]{2}(?::[0-9]{2}){2}\.[0-9]{3}Z",
                                                    timestamp))
                    admission.timestamp(timestamp)
                else:
                    if prop.attrib["name"] == admission.PRIVATE_CUTOFF_PROOF:
                        value = admission.private_cutoff_track(value)
                    fields = {"build_sha": sha, **attempt}
                    track, digest = value.get("track_id"), value.get("sha256")
                    require_review(value.keys() == fields.keys() | {"track_id", "sha256"}
                                   and isinstance(track, str) and admission.UUID.fullmatch(track)
                                   and isinstance(digest, str) and admission.HASH.fullmatch(digest))
                require_review(all(value[key] == expected_value for key, expected_value in fields.items()))
        observed.add(name)
    require_review(observed == expected.keys())
    # Render the checked-in names only. The validated first-attempt profile
    # represents passed cases, and every displayed case had to be in the report.
    return sorted(expected)


def write_review_summary(sha):
    attempt = provenance()
    cases = review_cases(read_bounded(REPORT, REVIEW_REPORT_LIMIT), sha, attempt)
    lines = [
        "## Private music contract enrollment review evidence",
        "",
        f"Workflow run `{attempt['run_id']}`, attempt `{attempt['run_attempt']}`, build `{sha}`.",
        "",
        "| Case (class `private-music`) | Observed first-attempt outcome |",
        "| --- | --- |",
        *(f"| `{name}` | passed |" for name in cases),
        "",
    ]
    with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a", encoding="utf-8") as summary:
        summary.write("\n".join(lines))


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
    admission.require(all(identity.valid(admission.UUID, receipt.get(key))
                          for key in ("version_id", "deployment_id")), "invalid staging deployment receipt IDs")
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


def attach_observations(report, deployments):
    xml = read_bounded(report, admission.MAX_XML)
    admission.require(not any(marker in xml for marker in (b"\x00", b"<!DOCTYPE", b"<!ENTITY")),
                      "unsafe private staging XML")
    suite = ET.fromstring(xml, parser=ET.XMLParser(encoding="utf-8"))
    for phase in admission.IDENTITY_PHASES:
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
    pins, deployments = deployment_pins(sha)
    if command == "observe":
        return observe(pins, provider(os.environ.get("SUBSONIC_STAGING_WORKERS_READ_TOKEN", "")))
    if command == "assemble":
        # Check the attempt before projecting the provider identities into JUnit.
        for phase in admission.IDENTITY_PHASES:
            observation = admission.parse_json(read_bounded(runner / f"{phase}.json", PIN_LIMIT), PIN_LIMIT)
            admission.require(observation == pins, "private staging boundary pins differ")
        attach_observations(REPORT, deployments)
        return pins
    raise ValueError


if __name__ == "__main__":
    try:
        command = sys.argv[1]
        if command == "review":
            sha = os.environ["GITHUB_SHA"]
            admission.require(admission.SHA.fullmatch(sha), "invalid private staging SHA")
            write_review_summary(sha)
        else:
            print(json.dumps(main(command), separators=(",", ":")))
    except (IndexError, KeyError, OSError, ValueError, RecursionError, ET.ParseError, admission.Refused, identity.Unavailable):
        if sys.argv[1:] == ["review"]:
            sys.exit("Private music contract enrollment review evidence unavailable.")
        sys.exit("Private staging deployment evidence unavailable or mismatched.")
