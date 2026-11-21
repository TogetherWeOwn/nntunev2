#!/usr/bin/env python3
"""Exercise synthetic restore after Actions step exits and abrupt process loss."""

import argparse
import json
import os
import runpy
import signal
import subprocess
import sys
import tempfile
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

    if scenario == "process-loss":
        # The file is closed; there must be no opportunity to write a stop receipt.
        os.kill(os.getpid(), signal.SIGKILL)
        raise AssertionError("SIGKILL returned")

    stop = "intentional-failure"
    if scenario == "timeout":
        stop = signal.sigwait(SIGNALS).name
    with state.with_suffix(".stop").open("x", encoding="ascii") as output:
        output.write(stop)
    return 23


class ProcessLossModel(RUNNER["StagingModel"]):
    """A synthetic backend outlives client cancellation and the off readback."""

    drain_at = 2 * RUNNER["TICK_MS"]

    def __init__(self):
        super().__init__()
        self.off_readback_at = None
        self.backend_samples = []

    def readback(self):
        deployment = super().readback()
        if deployment.verified_off() and self.off_readback_at is None:
            self.off_readback_at = self.now
        return deployment

    def telemetry(self):
        if self.pending or not self.state.verified_off():
            raise SystemExit("backend sampled before synthetic stop and restore")
        if self.off_readback_at is None or self.now <= self.off_readback_at:
            raise SystemExit("backend sampled at or before synthetic off readback")
        backends = int(self.now < self.drain_at)
        self.backend_samples.append((self.now, backends))
        return self.now, True, backends


def finalize(state, scenario, outcome):
    snapshot = json.loads(state.read_text(encoding="ascii"))
    process_loss = scenario == "process-loss"
    model = ProcessLossModel() if process_loss else RUNNER["StagingModel"]()
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
    if process_loss:
        expected_samples = [(RUNNER["TICK_MS"], 1), (model.drain_at, 0)]
        if (
            state.with_suffix(".stop").exists()
            or model.backend_samples != expected_samples
        ):
            raise SystemExit(
                "abrupt loss and independent backend drain were not verified"
            )
        print(
            "process-loss: fresh process verified synthetic deployment off and drained"
        )
        return 0
    stop = state.with_suffix(".stop").read_text(encoding="ascii")
    expected = (
        {"intentional-failure"} if scenario == "failure" else {s.name for s in SIGNALS}
    )
    if outcome != "failure" or stop not in expected:
        raise SystemExit("required Actions step exit was not observed")
    print(f"{scenario}: Actions reported failure; synthetic deployment off and drained")
    return 0


def rehearse_process_loss():
    script = [sys.executable, "-I", str(Path(__file__).resolve())]
    with tempfile.TemporaryDirectory() as tmp:
        state = str(Path(tmp) / "active.json")
        producer = subprocess.run(
            [*script, "arm", "process-loss", state], timeout=5, check=False
        )
        if producer.returncode != -signal.SIGKILL:
            raise SystemExit("synthetic producer did not exit via SIGKILL")
        subprocess.run(
            [*script, "finalize", "process-loss", state], timeout=5, check=True
        )
    return 0


def main():
    if sys.argv[1:] == ["rehearse-process-loss"]:
        return rehearse_process_loss()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("arm", "finalize"))
    parser.add_argument("scenario", choices=("failure", "timeout", "process-loss"))
    parser.add_argument("state", type=Path)
    parser.add_argument("--outcome")
    args = parser.parse_args()
    if args.phase == "arm":
        return arm(args.state, args.scenario)
    return finalize(args.state, args.scenario, args.outcome)


if __name__ == "__main__":
    raise SystemExit(main())
