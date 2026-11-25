#!/usr/bin/env python3
"""Check a private-launch observation record against independently collected pins."""

import argparse
from datetime import timedelta
from pathlib import Path
import re
import sys

from staging_admission import MAX_JSON, SHA, UUID, Refused, decimal, parse_json, require, timestamp


CADENCE = timedelta(minutes=5)
MINIMUM = timedelta(minutes=30)
UTC = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,6})?(?:Z|\+00:00)")
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,63}")
PRIVATE_LINK = re.compile(r"#evidence-[a-z0-9][a-z0-9-]{0,63}")
JOB_URL = re.compile(r"https://github\.com/TogetherWeOwn/nntunev2/actions/runs/([1-9][0-9]{0,15})/job/[1-9][0-9]{0,15}")
PROMOTION = {"sha", "promoted_version_id", "deployment_id", "run_id", "run_attempt"}
READBACKS = {"health": "passed", "readiness": "passed", "authorized_playback": "passed",
             "expected_denials": "passed", "web_errors": "within-threshold",
             "api_errors": "within-threshold", "stream_errors": "within-threshold"}


def matches(pattern, value):
    return isinstance(value, str) and pattern.fullmatch(value) is not None


def fields(value, expected, reason):
    require(isinstance(value, dict) and value.keys() == expected, reason)
    return value


def utc(value):
    require(matches(UTC, value), "observation time must be UTC")
    return timestamp(value)


def covered_by(person, start, close):
    fields(person, {"name", "from", "through", "recovery_access", "evidence"}, "missing watch assignment")
    require(matches(NAME, person["name"]) and person["recovery_access"] == "confirmed"
            and matches(PRIVATE_LINK, person["evidence"])
            and utc(person["from"]) <= start <= close <= utc(person["through"]),
            "watch assignment or recovery access does not cover the window")
    return person["name"].casefold()


def validate_observation(pins, watch):
    """Require complete, healthy evidence from final smoke through the closing readback."""
    fields(pins, {"schema", "candidate", "final_smoke", "error_policy"}, "invalid release pins")
    fields(watch, {"schema", "run_id", "run_attempt", "error_policy_evidence", "watcher", "backup",
                   "closed_at", "samples", "result"}, "invalid watch record")
    require(all(type(record["schema"]) is int and record["schema"] == 1 for record in (pins, watch)),
            "unsupported observation schema")
    candidate = fields(pins["candidate"], PROMOTION | {"api_sha"}, "incomplete candidate identity")
    require(all(matches(SHA, candidate[key]) for key in ("sha", "api_sha"))
            and all(matches(UUID, candidate[key]) for key in ("promoted_version_id", "deployment_id")),
            "invalid candidate identity")
    for key in ("run_id", "run_attempt"):
        require(candidate[key] == decimal(candidate[key]) == watch[key], "watch run or attempt mismatch")

    smoke = fields(pins["final_smoke"], PROMOTION | {"allocation_percent", "completed_at", "outcome", "evidence"},
                   "missing final smoke")
    job = JOB_URL.fullmatch(smoke["evidence"]) if isinstance(smoke["evidence"], str) else None
    require(smoke["outcome"] == "success" and job is not None and job[1] == candidate["run_id"]
            and type(smoke["allocation_percent"]) is int and smoke["allocation_percent"] == 100
            and all(smoke[key] == candidate[key] for key in PROMOTION),
            "final smoke did not succeed on the pinned allocation")
    start = utc(smoke["completed_at"])
    close = utc(watch["closed_at"])
    require(close - start >= MINIMUM and watch["result"] == "satisfied", "watch did not close successfully")

    policy = fields(pins["error_policy"], {"recorded_at", "evidence"}, "missing error baseline and thresholds")
    require(matches(PRIVATE_LINK, policy["evidence"]) and policy["evidence"] == watch["error_policy_evidence"]
            and utc(policy["recorded_at"]) < start, "error policy was not pinned before the watch")
    watcher = covered_by(watch["watcher"], start, close)
    require(watcher != covered_by(watch["backup"], start, close), "watcher and backup must be distinct")

    samples = watch["samples"]
    require(isinstance(samples, list) and samples, "missing observation samples")
    serving = {"web_sha": candidate["sha"], "api_sha": candidate["api_sha"],
               "promoted_version_id": candidate["promoted_version_id"], "deployment_id": candidate["deployment_id"],
               "allocation_percent": 100}
    previous = start
    for index, sample in enumerate(samples):
        fields(sample, serving.keys() | {"observed_at", "readbacks", "evidence"}, "incomplete observation sample")
        observed = utc(sample["observed_at"])
        if index == 0:
            require(start <= observed < start + CADENCE, "opening readback missed its deadline")
        else:
            require(timedelta(0) < observed - previous <= CADENCE, "invalid observation sample cadence")
        require(type(sample["allocation_percent"]) is int
                and all(sample[key] == expected for key, expected in serving.items()), "serving identity drift")
        require(sample["readbacks"] == READBACKS and matches(PRIVATE_LINK, sample["evidence"]),
                "readback failed, missing or unusable")
        previous = observed
    require(previous == close, "closing readback is missing")


def read_record(path):
    with path.open("rb") as handle:
        return parse_json(handle.read(MAX_JSON + 1))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pins", type=Path)
    parser.add_argument("watch", type=Path)
    args = parser.parse_args()
    try:
        validate_observation(read_record(args.pins), read_record(args.watch))
    except Refused as exc:
        print(f"Production observation refused: {exc}", file=sys.stderr)
        return 1
    except (OSError, RecursionError):
        print("Production observation refused: unreadable evidence", file=sys.stderr)
        return 1
    print("Production observation evidence satisfied")
    return 0


if __name__ == "__main__":
    sys.exit(main())
