"""Structural tests for the pnpm bootstrap retry in ci.yml.

The pinned pnpm self-installer runs one `npm ci` with no retry, so the
four pnpm-consuming jobs (svelte-check, vitest, web-build, web-e2e) wrap
it in a first attempt plus exactly one in-job retry. This suite pins that
shape: deleting a retry, masking it with continue-on-error, weakening its
gate, or drifting the pin between attempts must turn CI red.

Stdlib only: no committed check under .github/scripts depends on PyYAML,
so jobs and steps are located by indentation-aware scanning of the
workflow source instead of a YAML parse.

This module is also the shared workflow reader for the sibling
structural suites. It therefore cross-pins the web-e2e timeout guard's
registration in public-text: that suite cannot catch deletion of its
own run step, but this independently executed suite can (and the
timeout suite reciprocally pins this suite's step).
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
WEB_E2E_TIMEOUT_COMMAND = "python3 .github/scripts/test_ci_web_e2e_timeout.py"

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


def _key_token(field):
    """`field` as a YAML mapping key in any equivalent spelling.

    The same key can be written plain (``if:``), single- or
    double-quoted (``"if":``), with whitespace before the colon
    (``if :``), or with an explicit-key marker (``? if``, value on
    the following ``: value`` line). Absence checks fail open on any
    spelling they do not recognize, so every spelling counts.
    """
    plain = re.escape(field)
    return rf"(?:{plain}|\"{plain}\"|'{plain}')"


def _field_value(match):
    """Comment-stripped value for a key-token match.

    The bare explicit-key form (``? key`` with no colon) carries its
    value on the next line, so it reads as present-but-unknown: the
    empty string trips absence checks while failing positive reads.
    """
    value = match.group(1)
    if value is None:
        return ""
    return re.sub(r"\s+#.*$", "", value)


def job_level_field(job_chunk, field):
    """First `field: value` scalar at job-key indent (four spaces).

    ``step_field`` matches a field at any depth, so a key nested under
    a job mapping such as ``strategy.matrix`` or ``defaults`` can pose
    as the job's own field. Job keys sit at exactly four spaces (job
    ids at two); anything deeper belongs to a nested mapping. Quoted,
    space-before-colon, and explicit-key spellings of the same key
    count too: GitHub resolves them identically, so treating them as
    absent would let a masking key hide from the wiring checks.
    """
    key = _key_token(field)
    explicit = re.compile(rf"^    \?\s+{key}(?:\s*:\s*(.*?)\s*)?$")
    plain = re.compile(rf"^    {key}\s*:\s*(.*?)\s*$")
    for line in job_chunk.splitlines():
        match = explicit.match(line) or plain.match(line)
        if match:
            return _field_value(match)
    return None


def step_level_field(step_chunk, field):
    """First `field: value` scalar at step-key indent, comment-stripped.

    Step keys sit on the step's dash line (six spaces plus ``- ``) or
    at exactly eight spaces. Keys nested under a step mapping such as
    ``env:`` or ``with:`` sit deeper, so an ``env.run`` value cannot
    pose as the step's executed ``run`` command. Quoted,
    space-before-colon, and explicit-key spellings count too, for the
    same reason as in ``job_level_field``.
    """
    key = _key_token(field)
    prefix = r"(?:      -\s+|        )"
    explicit = re.compile(rf"^{prefix}\?\s+{key}(?:\s*:\s*(.*?)\s*)?$")
    plain = re.compile(rf"^{prefix}{key}\s*:\s*(.*?)\s*$")
    for line in step_chunk.splitlines():
        match = explicit.match(line) or plain.match(line)
        if match:
            return _field_value(match)
    return None


def job_needs(job_chunk):
    """Exact job ids from a job's `needs` (flow list, scalar, or block list).

    Substring membership on the raw scalar is not a gate check: a job id
    such as ``web-e2e-shadow`` contains ``web-e2e`` but does not make the
    dependent job wait for the browser suite.

    Only the job-level field counts (four-space indent): a ``needs``
    nested deeper, for instance a ``strategy.matrix.needs`` axis that
    happens to precede the real field, is matrix data, not a dependency.
    """
    lines = job_chunk.splitlines()
    for index, line in enumerate(lines):
        match = re.match(r"^(    )needs:\s*(.*?)\s*$", line)
        if not match:
            continue
        indent, value = match.groups()
        value = re.sub(r"\s+#.*$", "", value).strip()
        if value.startswith("["):
            flow = value
            while "]" not in flow and index + 1 < len(lines):
                index += 1
                flow += " " + re.sub(r"\s+#.*$", "", lines[index]).strip()
            inner = flow[1:flow.index("]")] if "]" in flow else flow[1:]
            return [item.strip().strip("'\"") for item in inner.split(",") if item.strip()]
        if value:
            return [value.strip("'\"")]
        needs = []
        for following in lines[index + 1:]:
            item = re.match(r"^(\s*)-\s*(.*?)\s*$", following)
            if item and len(item.group(1)) > len(indent):
                needs.append(re.sub(r"\s+#.*$", "", item.group(2)).strip().strip("'\""))
                continue
            if following.strip():
                break
        return needs
    return []


def check_required_step(job_id, job_chunk, command):
    """Error strings unless `command` runs as a required step of the job.

    Required means: a step whose `run` is exactly `command` (a trailing
    ``|| true`` or a shell wrapper would mask its failure), with no `if`
    gate and no `continue-on-error`, in a job that is itself neither
    gated nor continue-on-error. Anything weaker lets the guarded suite
    be silently dropped or neutralized without CI turning red.
    """
    errors = []
    # Job keys are unordered YAML, so `if` and `continue-on-error` count
    # on either side of `steps:`. The four-space pattern cannot match
    # inside the steps list, whose keys sit at six spaces or deeper.
    if job_level_field(job_chunk, "if") is not None:
        errors.append(f"{job_id}: job must be unconditional (no job-level if)")
    if job_level_field(job_chunk, "continue-on-error") is not None:
        errors.append(f"{job_id}: job must fail on a failed step, not continue-on-error")
    script = command.rsplit("/", 1)[-1]
    candidates = [
        step
        for step in split_steps(job_chunk)
        if script in (step_level_field(step, "run") or "")
    ]
    if not candidates:
        errors.append(f"{job_id}: no step runs {command}")
        return errors
    for step in candidates:
        if step_level_field(step, "run") != command:
            errors.append(f"{job_id}: step running {script} must be exactly `{command}`")
        elif step_level_field(step, "if") is not None:
            errors.append(f"{job_id}: step running {script} must be unconditional (no if)")
        elif step_level_field(step, "continue-on-error") is not None:
            errors.append(
                f"{job_id}: step running {script} must fail the job, "
                "not continue-on-error"
            )
        else:
            return errors
    return errors


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


def public_text_with(step_block, job_extra="", job_after_steps=""):
    """A minimal public-text job chunk carrying `step_block`.

    `job_extra` lands before the steps block, `job_after_steps` after it.
    """
    return (
        "  public-text:\n"
        "    runs-on: ubuntu-latest\n"
        f"{job_extra}"
        "    steps:\n"
        f"{step_block}"
        f"{job_after_steps}"
    )


def timeout_step(extra=""):
    """The web-e2e timeout guard's public-text step, plus `extra` lines."""
    return (
        "      - name: Structural-test the web-e2e timeout budget\n"
        f"        run: {WEB_E2E_TIMEOUT_COMMAND}\n"
        f"{extra}"
    )


def wiring_errors(text):
    """Wiring errors for the timeout guard in a public-text job chunk."""
    return check_required_step("public-text", text, WEB_E2E_TIMEOUT_COMMAND)


class TestWebE2eTimeoutWiring(unittest.TestCase):
    """Cross-pin the timeout guard's only CI registration.

    The timeout suite cannot catch deletion of its own run step, so this
    independently executed suite (same public-text job, separate step)
    requires that registration, and public-text's place in the ci-ok
    gate, to stay exactly as pinned.
    """

    def test_real_workflow_runs_timeout_guard_in_ci_ok_gate(self):
        """The real public-text job runs the guard and gates ci-ok."""
        with open(WORKFLOW_PATH, encoding="utf-8") as handle:
            jobs = split_jobs(handle.read())
        self.assertIn("public-text", jobs, "ci.yml no longer defines public-text")
        self.assertEqual(wiring_errors(jobs["public-text"]), [])
        self.assertIn("public-text", job_needs(jobs["ci-ok"]))

    def test_missing_timeout_step_fails(self):
        """Deleting the guard's run step turns this suite red."""
        errors = wiring_errors(public_text_with(""))
        self.assertTrue(any("no step runs" in e for e in errors), errors)

    def test_conditional_timeout_step_fails(self):
        """An `if` gate on the guard's step turns this suite red."""
        errors = wiring_errors(public_text_with(timeout_step("        if: always()\n")))
        self.assertTrue(any("must be unconditional" in e for e in errors), errors)

    def test_continue_on_error_timeout_step_fails(self):
        """A continue-on-error guard step turns this suite red."""
        errors = wiring_errors(
            public_text_with(timeout_step("        continue-on-error: true\n"))
        )
        self.assertTrue(any("not continue-on-error" in e for e in errors), errors)

    def test_masked_timeout_command_fails(self):
        """Masking the guard's exit code turns this suite red."""
        text = public_text_with(timeout_step()).replace(
            WEB_E2E_TIMEOUT_COMMAND, f"{WEB_E2E_TIMEOUT_COMMAND} || true"
        )
        errors = wiring_errors(text)
        self.assertTrue(any("must be exactly" in e for e in errors), errors)

    def test_nested_env_run_does_not_satisfy_required_step(self):
        """An exact nested `env.run` is not the executed command.

        The same scoped reader guards the reciprocal pnpm-command pin
        the timeout suite makes, so both commands are checked here.
        """
        for command in (
            WEB_E2E_TIMEOUT_COMMAND,
            "python3 .github/scripts/test_ci_pnpm_retry.py",
        ):
            with self.subTest(command=command):
                step = (
                    "      - name: Structural-test the web-e2e timeout budget\n"
                    "        env:\n"
                    f"          run: {command}\n"
                    "        run: echo skipped\n"
                )
                errors = check_required_step(
                    "public-text", public_text_with(step), command
                )
                self.assertTrue(any("no step runs" in e for e in errors), errors)

    def test_nested_env_run_does_not_mask_real_timeout_step(self):
        """A nested `env.run` before the real command is ignored."""
        step = (
            "      - name: Structural-test the web-e2e timeout budget\n"
            "        env:\n"
            "          run: echo not-the-command\n"
            f"        run: {WEB_E2E_TIMEOUT_COMMAND}\n"
        )
        self.assertEqual(wiring_errors(public_text_with(step)), [])

    def test_dash_line_run_satisfies_required_step(self):
        """A command on the step's dash line is a step-level `run`."""
        step = f"      - run: {WEB_E2E_TIMEOUT_COMMAND}\n"
        self.assertEqual(wiring_errors(public_text_with(step)), [])

    def test_job_level_continue_on_error_fails(self):
        """A continue-on-error public-text job turns this suite red."""
        errors = wiring_errors(
            public_text_with(timeout_step(), job_extra="    continue-on-error: true\n")
        )
        self.assertTrue(any("not continue-on-error" in e for e in errors), errors)

    def test_job_level_keys_after_steps_fail(self):
        """Job-level `if`/`continue-on-error` after the steps block count too."""
        cases = (
            ("    continue-on-error: true\n", "not continue-on-error"),
            ("    if: ${{ false }}\n", "no job-level if"),
        )
        for after, expected in cases:
            with self.subTest(after=after):
                errors = wiring_errors(
                    public_text_with(timeout_step(), job_after_steps=after)
                )
                self.assertTrue(any(expected in e for e in errors), errors)

    def test_step_keys_are_not_job_level(self):
        """Step-level `if` text deeper than job keys stays a step concern."""
        step = timeout_step() + "      - name: later\n        if: always()\n        run: echo ok\n"
        self.assertEqual(wiring_errors(public_text_with(step)), [])

    # Alternate YAML spellings of one `{key}: {value}` line. The
    # explicit-key form carries its value on the next `: value` line.
    ALTERNATE_SPELLINGS = (
        '"{key}": {value}\n',
        "'{key}': {value}\n",
        "{key} : {value}\n",
        '"{key}" : {value}\n',
        "? {key}\n{indent}: {value}\n",
        '? "{key}"\n{indent}: {value}\n',
    )
    MASKING_KEYS = (
        ("continue-on-error", "true", "not continue-on-error"),
        ("if", "${{ false }}", "no job-level if"),
    )

    def test_alternate_job_key_spellings_fail(self):
        """Every spelling of a masking job-level key turns this suite red."""
        for key, value, expected in self.MASKING_KEYS:
            for spelling in self.ALTERNATE_SPELLINGS:
                with self.subTest(key=key, spelling=spelling):
                    line = "    " + spelling.format(
                        key=key, value=value, indent="    "
                    )
                    errors = wiring_errors(
                        public_text_with(timeout_step(), job_extra=line)
                    )
                    self.assertTrue(any(expected in e for e in errors), errors)

    def test_alternate_step_key_spellings_fail(self):
        """Every spelling of a masking step-level key turns this suite red."""
        for key, value, expected in (
            ("continue-on-error", "true", "not continue-on-error"),
            ("if", "always()", "must be unconditional"),
        ):
            for spelling in self.ALTERNATE_SPELLINGS:
                with self.subTest(key=key, spelling=spelling):
                    line = "        " + spelling.format(
                        key=key, value=value, indent="        "
                    )
                    errors = wiring_errors(
                        public_text_with(timeout_step(line))
                    )
                    self.assertTrue(any(expected in e for e in errors), errors)

    def test_quoted_job_key_after_steps_fails(self):
        """Spelling and placement compose: quoted key after steps counts."""
        errors = wiring_errors(
            public_text_with(
                timeout_step(), job_after_steps='    "continue-on-error": true\n'
            )
        )
        self.assertTrue(any("not continue-on-error" in e for e in errors), errors)


if __name__ == "__main__":
    unittest.main()
