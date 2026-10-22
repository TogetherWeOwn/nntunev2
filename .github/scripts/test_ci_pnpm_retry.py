"""Structural tests for the pnpm bootstrap retry in ci.yml.

The pinned pnpm self-installer runs one `npm ci` with no retry, so the
four pnpm-consuming jobs (svelte-check, vitest, web-build, web-e2e) wrap
it in a first attempt plus exactly one in-job retry. This suite pins that
shape: deleting a retry, masking it with continue-on-error, weakening its
gate, or drifting the pin between attempts must turn CI red.

Stdlib only: no committed check under .github/scripts depends on PyYAML,
so jobs and steps are located by indentation-aware scanning of the
workflow source instead of a YAML parse.
"""

import os
import re
import unittest

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
WORKFLOW_PATH = os.path.join(REPO_ROOT, ".github", "workflows", "ci.yml")

RETRY_JOBS = ("svelte-check", "vitest", "web-build", "web-e2e")
RETRY_STEP_NAME = "Retry pnpm bootstrap once"
RETRY_GATE = "steps.pnpm-setup.outcome == 'failure'"
PINNED_USES = re.compile(r"^pnpm/action-setup@[0-9a-f]{40}$")

_JOB_HEADER = re.compile(r"^  ([A-Za-z0-9_-]+):\s*(?:#.*)?$")
_STEPS_HEADER = re.compile(r"^    steps:\s*(?:#.*)?$")
_JOB_KEY = re.compile(r"^    [A-Za-z]")
_STEP_START = re.compile(r"^      -(?:\s|$)")


def split_jobs(text):
    """Map job id -> job chunk for the workflow's jobs block."""
    jobs = {}
    current = None
    in_jobs = False
    for line in text.splitlines():
        if re.match(r"^jobs:\s*(?:#.*)?$", line):
            in_jobs = True
            continue
        if not in_jobs:
            continue
        header = _JOB_HEADER.match(line)
        if header:
            current = header.group(1)
            jobs[current] = []
        elif re.match(r"^[A-Za-z]", line):
            current = None
        if current is not None:
            jobs[current].append(line)
    return {job: "\n".join(lines) for job, lines in jobs.items()}


def split_steps(job_chunk):
    """Split a job chunk's steps block into per-step chunks."""
    steps = []
    current = None
    in_steps = False
    for line in job_chunk.splitlines():
        if not in_steps:
            if _STEPS_HEADER.match(line):
                in_steps = True
            continue
        if _STEP_START.match(line):
            current = [line]
            steps.append(current)
        elif current is None:
            continue
        elif _JOB_KEY.match(line):
            break
        else:
            current.append(line)
    return ["\n".join(step) for step in steps]


def step_field(step_chunk, field):
    """First `field: value` scalar in a step chunk, comment-stripped."""
    pattern = re.compile(rf"^\s*(?:-\s+)?{re.escape(field)}:\s*(.*?)\s*$")
    for line in step_chunk.splitlines():
        match = pattern.match(line)
        if match:
            return re.sub(r"\s+#.*$", "", match.group(1))
    return None


def check_job_retry(job_id, job_chunk):
    """Error strings for a job's pnpm retry shape; empty means pinned."""
    errors = []
    steps = split_steps(job_chunk)
    first_index = next(
        (i for i, step in enumerate(steps) if step_field(step, "id") == "pnpm-setup"),
        None,
    )
    if first_index is None:
        return [f"{job_id}: no step with id pnpm-setup"]
    first = steps[first_index]
    if step_field(first, "continue-on-error") != "true":
        errors.append(f"{job_id}: first pnpm-setup attempt must set continue-on-error")
    retry_index = next(
        (
            i
            for i in range(first_index + 1, len(steps))
            if step_field(steps[i], "name") == RETRY_STEP_NAME
        ),
        None,
    )
    if retry_index is None:
        errors.append(f"{job_id}: no '{RETRY_STEP_NAME}' step after pnpm-setup")
        return errors
    retry = steps[retry_index]
    if step_field(retry, "if") != RETRY_GATE:
        errors.append(f"{job_id}: retry gate must be exactly `{RETRY_GATE}`")
    if step_field(retry, "continue-on-error") == "true":
        errors.append(f"{job_id}: retry must fail the job, not continue-on-error")
    first_uses = step_field(first, "uses")
    retry_uses = step_field(retry, "uses")
    if not first_uses or not PINNED_USES.match(first_uses):
        errors.append(f"{job_id}: first attempt must pin pnpm/action-setup to a full SHA")
    if retry_uses != first_uses:
        errors.append(f"{job_id}: retry must reuse the first attempt's pinned uses")
    first_version = step_field(first, "version")
    if not first_version:
        errors.append(f"{job_id}: first attempt must set a pnpm version input")
    elif step_field(retry, "version") != first_version:
        errors.append(f"{job_id}: retry must reuse the first attempt's version input")
    return errors


TEMPLATE = """jobs:
  demo:
    runs-on: ubuntu-latest
    steps:
{steps}
"""


def workflow_with(first_extra="", retry_step=True, retry_extra="", first_sha="e" * 40, retry_sha="e" * 40):
    first = (
        "      - id: pnpm-setup\n"
        f"        uses: pnpm/action-setup@{first_sha}\n"
        "        continue-on-error: true\n"
        "        with:\n"
        "          version: 10\n"
        f"{first_extra}"
    )
    retry = (
        "      - name: Retry pnpm bootstrap once\n"
        "        if: steps.pnpm-setup.outcome == 'failure'\n"
        f"        uses: pnpm/action-setup@{retry_sha}\n"
        "        with:\n"
        "          version: 10\n"
        f"{retry_extra}"
    )
    steps = first + ("\n" + retry if retry_step else "")
    return TEMPLATE.format(steps=steps)


def errors_for(text):
    return check_job_retry("demo", split_jobs(text)["demo"])


class TestPnpmRetryShape(unittest.TestCase):
    def test_real_workflow_pins_all_four_retries(self):
        with open(WORKFLOW_PATH, encoding="utf-8") as handle:
            jobs = split_jobs(handle.read())
        for job_id in RETRY_JOBS:
            self.assertIn(job_id, jobs, f"ci.yml no longer defines job {job_id}")
            self.assertEqual(check_job_retry(job_id, jobs[job_id]), [])

    def test_missing_retry_fails(self):
        errors = errors_for(workflow_with(retry_step=False))
        self.assertTrue(any("no 'Retry pnpm bootstrap once'" in e for e in errors), errors)

    def test_retry_with_continue_on_error_fails(self):
        errors = errors_for(workflow_with(retry_extra="        continue-on-error: true\n"))
        self.assertTrue(any("not continue-on-error" in e for e in errors), errors)

    def test_weakened_retry_gate_fails(self):
        text = workflow_with().replace(RETRY_GATE, "always()")
        errors = errors_for(text)
        self.assertTrue(any("must be exactly" in e for e in errors), errors)

    def test_pin_drift_between_attempts_fails(self):
        errors = errors_for(workflow_with(retry_sha="f" * 40))
        self.assertTrue(any("pinned uses" in e for e in errors), errors)

    def test_floating_tag_fails(self):
        text = re.sub(r"pnpm/action-setup@[0-9a-f]{40}", "pnpm/action-setup@v6", workflow_with())
        errors = errors_for(text)
        self.assertTrue(any("full SHA" in e for e in errors), errors)

    def test_first_attempt_without_continue_on_error_fails(self):
        text = workflow_with().replace("        continue-on-error: true\n", "", 1)
        errors = errors_for(text)
        self.assertTrue(any("must set continue-on-error" in e for e in errors), errors)


if __name__ == "__main__":
    unittest.main()
