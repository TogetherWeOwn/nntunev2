#!/usr/bin/env python3
"""Exercise the synthetic restore model across actual Actions step exits."""

import argparse
import json
import runpy
import signal
from dataclasses import asdict
from pathlib import Path

RUNNER = runpy.run_path(str(Path(__file__).with_name("subsonic-staging-runner.py")))
SIGNALS = {signal.SIGINT, signal.SIGTERM}


def arm(state, scenario):
    # Block before recording the arm so there is no race between publishing
    # the active state and waiting for the native runner signal.
    if scenario == "timeout":
        signal.pthread_sigmask(signal.SIG_BLOCK, SIGNALS)
    model = RUNNER["StagingModel"]()
    model.arm()
    model.start({"slot": 1})
    with state.open("x", encoding="ascii") as output:
        json.dump({"deployment": asdict(model.state), "pending": model.pending}, output)

    stop = "intentional-failure"
    if scenario == "timeout":
        stop = signal.sigwait(SIGNALS).name
    with state.with_suffix(".stop").open("x", encoding="ascii") as output:
        output.write(stop)
    return 23


def finalize(state, scenario, outcome):
    snapshot = json.loads(state.read_text(encoding="ascii"))
    model = RUNNER["StagingModel"]()
    model.state = RUNNER["Deployment"](**snapshot["deployment"])
    model.pending = {int(slot): at for slot, at in snapshot["pending"].items()}
    started = model.state == RUNNER["EXPERIMENT"] and len(model.pending) == 1
    limits = RUNNER["load_matrix"](RUNNER["MATRIX"])["limits"]
    # Run cleanup even if the step did not end the way the test expected.
    restored = RUNNER["cleanup"](model, limits)
    if not started or restored != "simulated-off-and-drained":
        raise SystemExit("synthetic restore and drain were not verified")
    if not model.state.verified_off() or model.pending:
        raise SystemExit("synthetic deployment or client remained active")
    stop = state.with_suffix(".stop").read_text(encoding="ascii")
    expected = (
        {"intentional-failure"} if scenario == "failure" else {s.name for s in SIGNALS}
    )
    if outcome != "failure" or stop not in expected:
        raise SystemExit("required Actions step exit was not observed")
    print(f"{scenario}: Actions reported failure; synthetic deployment off and drained")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("arm", "finalize"))
    parser.add_argument("scenario", choices=("failure", "timeout"))
    parser.add_argument("state", type=Path)
    parser.add_argument("--outcome")
    args = parser.parse_args()
    if args.phase == "arm":
        return arm(args.state, args.scenario)
    return finalize(args.state, args.scenario, args.outcome)


if __name__ == "__main__":
    raise SystemExit(main())
