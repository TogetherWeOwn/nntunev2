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
timeout suite reciprocally pins this suite's step). It also independently
pins both namespace/staging banks in tail-projection and checks that ci-ok
propagates their failures; deleting a bank cannot delete its only assertion.
"""

import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

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

    The guarded layout requires one ASCII space after the step dash,
    so step keys sit on that line or at exactly eight spaces. Other
    layouts are rejected by ``non_plain_key_lines``. Keys nested under
    a step mapping such as
    ``env:`` or ``with:`` sit deeper, so an ``env.run`` value cannot
    pose as the step's executed ``run`` command. Quoted,
    space-before-colon, and explicit-key spellings count too, for the
    same reason as in ``job_level_field``. A deeper non-comment line
    before the next key is scalar continuation content. Retain a newline
    and that content so it cannot equal a required single-line command;
    this reader deliberately rejects multiline commands instead of
    approximating YAML folding.
    """
    key = _key_token(field)
    prefix = r"(?:      - |        )"
    explicit = re.compile(rf"^{prefix}\?\s+{key}(?:\s*:\s*(.*?)\s*)?$")
    plain = re.compile(rf"^{prefix}{key}\s*:\s*(.*?)\s*$")
    lines = step_chunk.splitlines()
    for index, line in enumerate(lines):
        match = explicit.match(line) or plain.match(line)
        if match:
            value = _field_value(match)
            for following in lines[index + 1:]:
                if not following.strip() or following.lstrip().startswith("#"):
                    continue
                if len(following) - len(following.lstrip(" \t")) > 8:
                    return value + "\n" + following.strip()
                break
            return value
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


_PLAIN_KEY = r"[A-Za-z_][A-Za-z0-9_-]*:(?:\s|$)"
_JOB_KEY_LINE = re.compile(rf"^    (?!\s)(.*)$")
_STEP_DASH_LINE = re.compile(r"^      -(.*)$")
_STEP_KEY_LINE = re.compile(r"^        (?!\s)(.*)$")


def non_plain_key_lines(job_chunk):
    """Job- and step-key lines not written as a plain `key:` mapping key.

    ``job_level_field``/``step_level_field`` can only recognize the
    spellings they enumerate, and YAML has more than anyone lists:
    escaped double-quoted keys, anchored or tagged keys, block-scalar
    explicit keys, flow-mapping steps, merge keys. A guard that fails
    open on an unknown spelling lets a masking ``if`` or
    ``continue-on-error`` hide. So the required job and its steps may
    use only plain ``key:`` at job- and step-key indent, with exactly
    one ASCII space after a step dash. Consuming arbitrary whitespace
    there would shift subsequent keys beyond the eight-space reader.
    Anything else is reported and fails closed instead of being parsed.
    """
    bad = []
    for line in job_chunk.splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        dash = _STEP_DASH_LINE.match(line)
        if dash:
            rest = dash.group(1)
            if rest and (
                not rest.startswith(" ") or not re.match(_PLAIN_KEY, rest[1:])
            ):
                bad.append(line)
            continue
        for pattern in (_JOB_KEY_LINE, _STEP_KEY_LINE):
            match = pattern.match(line)
            if match and not re.match(_PLAIN_KEY, match.group(1)):
                bad.append(line)
                break
    return bad


def defaults_shell(chunk, indent):
    """Read only block-form defaults.run.shell; unknown layouts fail closed.

    None means no shell default; an empty string means present but unsupported.
    As with the job/step readers, this deliberately supports a bounded YAML
    layout rather than approximating flow mappings, anchors or scalar folding.
    """
    lines = chunk.splitlines()
    for field in ("defaults", "run", "shell"):
        content = [line for line in lines if line.strip() and not line.lstrip().startswith("#")]
        if field != "defaults" and content:
            if min(len(line) - len(line.lstrip(" ")) for line in content) != indent:
                return ""
        matches = []
        for index, line in enumerate(lines):
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            if len(line) - len(line.lstrip(" ")) != indent:
                continue
            key = re.fullmatch(r" *([A-Za-z_][A-Za-z0-9_-]*):\s*(.*?)\s*", line)
            if not key:
                return ""
            if key.group(1) == field:
                matches.append((index, re.sub(r"\s+#.*$", "", key.group(2))))
        if not matches:
            return None
        if len(matches) != 1:
            return ""
        index, value = matches[0]
        end = index + 1
        while end < len(lines):
            following = lines[end]
            if following.strip() and not following.lstrip().startswith("#"):
                if len(following) - len(following.lstrip(" ")) <= indent:
                    break
            end += 1
        children = lines[index + 1:end]
        if field == "shell":
            return "" if any(s.strip() and not s.lstrip().startswith("#") for s in children) else value
        if value:
            return ""
        lines = children
        indent += 2
    return None


SUPPORTED_SHELLS = {
    "bash": "bash --noprofile --norc -e -o pipefail {0}",
    "sh": "sh -e {0}",
    "bash -e {0}": "bash -e {0}",
    "sh -e {0}": "sh -e {0}",
    "bash --noprofile --norc -e -o pipefail {0}": "bash --noprofile --norc -e -o pipefail {0}",
}


def step_env_field(step, field):
    """Read a scalar only from the step's block-form env mapping.

    Nested with/name/run content cannot supply an environment binding. Unknown,
    duplicate or multiline mapping layouts fail closed instead of guessing YAML.
    """
    lines = step.splitlines()
    envs = [index for index, line in enumerate(lines)
            if re.match(r"^(?:      - |        )env:", line)]
    if not envs:
        return None
    if len(envs) != 1 or not re.fullmatch(
        r"(?:      - |        )env:\s*(?:#.*)?", lines[envs[0]],
    ):
        return ""
    values = {}
    for line in lines[envs[0] + 1:]:
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if len(line) - len(line.lstrip(" ")) <= 8:
            break
        match = re.fullmatch(r"          ([A-Za-z_][A-Za-z0-9_]*):\s*(.*?)\s*", line)
        if not match or match.group(1) in values:
            return ""
        values[match.group(1)] = re.sub(r"\s+#.*$", "", match.group(2))
    return values.get(field)


def replace_mapping_field(chunk, field, replacement, indent, *, insert_at=0):
    """Replace/remove a plain fixture field and its block, never append a duplicate."""
    lines = chunk.splitlines()
    pattern = re.compile(rf"^{' ' * indent}{re.escape(field)}:\s*")
    kept = []
    index = 0
    position = None
    while index < len(lines):
        if not pattern.match(lines[index]):
            kept.append(lines[index])
            index += 1
            continue
        if position is None:
            position = len(kept)
        index += 1
        while index < len(lines):
            line = lines[index]
            if line.strip() and len(line) - len(line.lstrip(" ")) <= indent:
                break
            index += 1
    if replacement:
        position = insert_at if position is None else position
        kept[position:position] = replacement.splitlines()
    return "\n".join(kept) + ("\n" if chunk.endswith("\n") else "")


def with_step_field(step, field, value):
    """Set/remove a step fixture key, including when it was the dash-line key."""
    lines = step.splitlines()
    lines[0] = "        " + lines[0][8:]
    replacement = "" if value is None else f"        {field}: {value}"
    changed = replace_mapping_field("\n".join(lines), field, replacement, 8).splitlines()
    for index, line in enumerate(changed):
        if line.strip() and not line.lstrip().startswith("#"):
            changed[index] = "      - " + line[8:]
            return "\n".join(changed)
    raise AssertionError("Step fixture cannot be empty")


def with_defaults_shell(chunk, shell, indent):
    """Replace/remove a fixture's entire defaults block at the selected scope."""
    prefix = " " * indent
    replacement = "" if shell is None else (
        f"{prefix}defaults:\n{prefix}  run:\n{prefix}    shell: {shell}"
    )
    return replace_mapping_field(chunk, "defaults", replacement, indent,
                                 insert_at=0 if indent == 0 else 1)


def effective_shell(workflow_text, job_chunk, step):
    """Resolve step > job > workflow > Linux/container default without executing text."""
    value = step_level_field(step, "shell")
    if value is None:
        value = defaults_shell(job_chunk, 4)
    if value is None:
        value = defaults_shell(workflow_text, 0)
    if value is None:
        return "sh -e {0}" if job_level_field(job_chunk, "container") is not None else "bash -e {0}"
    if value.startswith('"'):
        try:
            value = json.loads(value)
        except ValueError:
            return ""
    elif value.startswith("'") and value.endswith("'"):
        value = value[1:-1].replace("''", "'")
    return SUPPORTED_SHELLS.get(value, "")


def check_step_shell(job_id, job_chunk, step, workflow_text=""):
    if effective_shell(workflow_text, job_chunk, step):
        return []
    return [f"{job_id}: step must use a supported effective bash/sh shell that executes {{0}}"]


def check_required_step(job_id, job_chunk, command, *, workflow_text=""):
    """Error strings unless `command` runs as a required step of the job.

    Required means: a step whose `run` is exactly `command` (a trailing
    ``|| true`` or a shell wrapper would mask its failure), with no `if`
    gate and no `continue-on-error`, in a job that is itself neither
    gated nor continue-on-error. The effective shell must execute its
    script and propagate failure. Pass workflow_text for workflow-level
    defaults; callers with isolated job fixtures have no inherited default.
    Anything weaker lets the suite be neutralized without CI turning red.
    """
    errors = [
        f"{job_id}: key line must be a plain `key:` with exactly one ASCII "
        f"space after a step dash (other layouts can hide masking keys): {line.strip()!r}"
        for line in non_plain_key_lines(job_chunk)
    ]
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
            errors.extend(check_step_shell(job_id, job_chunk, step, workflow_text))
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


    # Spellings GitHub's parser resolves to the same key but that no
    # enumerated regex can be trusted to cover: the guard rejects any
    # non-plain key line instead of decoding it.
    EXOTIC_JOB_LINES = (
        '    "continue-on\\u002derror": true\n',
        '    "i\\x66": ${{ false }}\n',
        "    &a continue-on-error: true\n",
        "    !!str continue-on-error: true\n",
        "    ? |-\n      continue-on-error\n    : true\n",
        "    <<: {continue-on-error: true}\n",
        "    - indentless\n",
    )
    EXOTIC_STEP_LINES = (
        '        "continue-on\\u002derror": true\n',
        "        &a continue-on-error: true\n",
        "        !!str if: always()\n",
        "        ? |-\n          if\n        : always()\n",
    )

    def test_exotic_job_key_spellings_fail_closed(self):
        """Escaped, anchored, tagged, block-scalar, merge keys are rejected."""
        for line in self.EXOTIC_JOB_LINES:
            for placement in ("job_extra", "job_after_steps"):
                with self.subTest(line=line, placement=placement):
                    errors = wiring_errors(
                        public_text_with(timeout_step(), **{placement: line})
                    )
                    self.assertTrue(any("plain `key:`" in e for e in errors), errors)

    def test_exotic_step_key_spellings_fail_closed(self):
        """Exotic step-level spellings are rejected, even composed with a masked run."""
        for line in self.EXOTIC_STEP_LINES:
            with self.subTest(line=line):
                errors = wiring_errors(public_text_with(timeout_step(line)))
                self.assertTrue(any("plain `key:`" in e for e in errors), errors)

    def test_flow_mapping_and_tagged_step_start_fail_closed(self):
        """A step written as a flow mapping or with a node property is rejected."""
        for first in (
            '{"if": false, run: ' + WEB_E2E_TIMEOUT_COMMAND + "}",
            "&s name: guard",
            "!!str name: guard",
            '"name": guard',
        ):
            with self.subTest(first=first):
                step = f"      - {first}\n        run: {WEB_E2E_TIMEOUT_COMMAND}\n"
                errors = wiring_errors(public_text_with(step))
                self.assertTrue(any("plain `key:`" in e for e in errors), errors)

    def test_folded_run_continuations_fail_closed(self):
        """A later physical line is part of the command, not ignorable data."""
        for command in (
            WEB_E2E_TIMEOUT_COMMAND,
            "python3 .github/scripts/test_ci_pnpm_retry.py",
        ):
            for dash_run in (False, True):
                first = (
                    f"      - run: {command}\n" if dash_run else
                    f"      - name: guard\n        run: {command}\n"
                )
                for indent in (9, 10, 12, 16):
                    for prefix in ("", "\n", "          # note\n\n"):
                        with self.subTest(
                            command=command, dash_run=dash_run,
                            indent=indent, prefix=prefix,
                        ):
                            step = first + prefix + " " * indent + "|| true\n"
                            errors = check_required_step(
                                "public-text", public_text_with(step), command
                            )
                            self.assertTrue(any("must be exactly" in e for e in errors), errors)

    def test_single_line_run_with_later_nested_data_stays_clean(self):
        """Comments, blanks and a subsequent mapping are not continuations."""
        step = (
            f"      - run: {WEB_E2E_TIMEOUT_COMMAND}\n"
            "          # a comment is not scalar content\n\n"
            "        env:\n"
            "          NOTE: harmless nested data\n"
            "      - run: echo later-step\n"
        )
        self.assertEqual(wiring_errors(public_text_with(step)), [])

    def test_folded_pnpm_mask_fails_reciprocal_whole_workflow_suite(self):
        """Execute the surviving suite against the composed real-workflow mutant."""
        with open(WORKFLOW_PATH, encoding="utf-8") as handle:
            text = handle.read()
        command = "python3 .github/scripts/test_ci_pnpm_retry.py"
        line = f"        run: {command}\n"
        self.assertEqual(text.count(line), 1)
        mutant = text.replace(line, line + "          || true\n").replace(
            RETRY_GATE, "steps.pnpm-setup.outcome == 'success'", 1
        )
        with tempfile.TemporaryDirectory() as root:
            scripts = os.path.join(root, ".github", "scripts")
            workflows = os.path.join(root, ".github", "workflows")
            os.makedirs(scripts)
            os.makedirs(workflows)
            for name in ("test_ci_pnpm_retry.py", "test_ci_web_e2e_timeout.py"):
                shutil.copyfile(os.path.join(REPO_ROOT, ".github", "scripts", name),
                                os.path.join(scripts, name))
            with open(os.path.join(workflows, "ci.yml"), "w", encoding="utf-8") as handle:
                handle.write(mutant)
            result = subprocess.run(
                [sys.executable, os.path.join(scripts, "test_ci_web_e2e_timeout.py")],
                capture_output=True, text=True, timeout=30,
            )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("test_pnpm_guard_step_is_required_in_public_text", result.stderr)
        self.assertIn("must be exactly", result.stderr)

    def test_extra_dash_spacing_fails_closed(self):
        """Reject layouts that move masking keys past the eight-space reader."""
        for command in (
            WEB_E2E_TIMEOUT_COMMAND,
            "python3 .github/scripts/test_ci_pnpm_retry.py",
        ):
            for spaces in (2, 3, 4, 8):
                indent = " " * (7 + spaces)
                for key_line in (
                    "continue-on-error: true",
                    "if: false",
                    '"continue-on\\u002derror": true',
                    "&a continue-on-error: true",
                    "!!str if: false",
                ):
                    for first_key in ("run", "name"):
                        with self.subTest(
                            command=command, spaces=spaces,
                            key_line=key_line, first_key=first_key,
                        ):
                            first = f"run: {command}" if first_key == "run" else "name: guard"
                            step = "      -" + " " * spaces + first + "\n"
                            if first_key != "run":
                                step += f"{indent}run: {command}\n"
                            step += f"{indent}{key_line}\n"
                            errors = check_required_step(
                                "public-text", public_text_with(step), command
                            )
                            self.assertTrue(
                                any("plain `key:`" in e for e in errors), errors
                            )

    def test_canonical_dash_spacing_stays_clean(self):
        """A one-space dash and nested data remain valid for both commands."""
        for command in (
            WEB_E2E_TIMEOUT_COMMAND,
            "python3 .github/scripts/test_ci_pnpm_retry.py",
        ):
            with self.subTest(command=command):
                step = (
                    f"      - run: {command}\n"
                    "        env:\n"
                    "          if: data-not-a-step-gate\n"
                    "          continue-on-error: data-not-a-step-control\n"
                )
                self.assertEqual(
                    check_required_step("public-text", public_text_with(step), command), []
                )

    def test_plain_keys_comments_and_blank_lines_stay_clean(self):
        """Negative control: plain keys, comments and blanks are not flagged."""
        step = (
            "      # note\n\n"
            "      - name: guard\n"
            "        # note\n"
            "        env:\n          A: b\n"
            f"        run: {WEB_E2E_TIMEOUT_COMMAND}\n"
        )
        self.assertEqual(non_plain_key_lines(public_text_with(step, job_extra="    # c\n")), [])
        self.assertEqual(wiring_errors(public_text_with(step)), [])


class TestEffectiveShell(unittest.TestCase):
    def test_container_defaults_to_sh_unless_a_more_specific_shell_is_set(self):
        step = timeout_step()
        job = public_text_with(step, job_extra="    container: fixture-image\n")
        self.assertEqual(effective_shell("", job, step), "sh -e {0}")
        for explicit in ("bash", "sh"):
            with self.subTest(shell=explicit):
                overridden = timeout_step(f"        shell: {explicit}\n")
                self.assertEqual(effective_shell("", job, overridden), SUPPORTED_SHELLS[explicit])
        inherited = "defaults:\n  run:\n    shell: bash\n"
        self.assertEqual(effective_shell(inherited, job, step), SUPPORTED_SHELLS["bash"])

    def test_every_supported_invocation_executes_the_script_and_preserves_failure(self):
        for shell in (None, *SUPPORTED_SHELLS):
            step = timeout_step("" if shell is None else f"        shell: {shell}\n")
            job = public_text_with(step)
            invocation = effective_shell("", job, step)
            self.assertTrue(invocation)
            with self.subTest(shell=shell), tempfile.TemporaryDirectory() as root:
                script = os.path.join(root, "fixture.sh")
                marker = os.path.join(root, "executed")
                args = [script if part == "{0}" else part for part in shlex.split(invocation)]
                for exit_code in (0, 9):
                    with open(script, "w", encoding="utf-8") as handle:
                        handle.write(f"printf ran > executed\nexit {exit_code}\n")
                    result = subprocess.run(args, cwd=root, env={"PATH": os.defpath},
                                            capture_output=True, text=True, timeout=5)
                    self.assertEqual(result.returncode, exit_code, result.stdout + result.stderr)
                    with open(marker, encoding="utf-8") as handle:
                        self.assertEqual(handle.read(), "ran")
                    os.unlink(marker)

    def test_nonexecuting_shells_and_unsupported_yaml_defaults_fail_closed(self):
        step = timeout_step()
        for shell in ("true {0}", "bash -c true {0}", "bash -e", "sh {0}",
                      "bash -e {0} || true", "${{ env.SHELL }}", "null", ""):
            with self.subTest(shell=shell):
                masked = timeout_step(f"        shell: {shell}\n")
                self.assertTrue(check_step_shell("public-text", public_text_with(masked), masked))
        for defaults in (
            "defaults: {run: {shell: 'true {0}'}}\n",
            "defaults:\n  run: {shell: 'true {0}'}\n",
            'defaults:\n  run:\n    "shell": true {0}\n',
            "defaults:\n  run:\n    <<: {shell: 'true {0}'}\n",
            "defaults:\n    run:\n      shell: true {0}\n",
            "defaults:\n run:\n  shell: true {0}\n",
            "defaults:\n  run:\n    shell: bash\n      -c true {0}\n",
            "defaults:\n  run:\n    shell: bash\ndefaults:\n  run:\n    shell: true {0}\n",
        ):
            with self.subTest(defaults=defaults):
                self.assertTrue(check_step_shell(
                    "public-text", public_text_with(step), step, defaults,
                ))
                job_defaults = "\n".join("    " + line for line in defaults.splitlines()) + "\n"
                self.assertTrue(check_step_shell(
                    "public-text", public_text_with(step, job_extra=job_defaults), step,
                ))

    def test_unrelated_nested_shell_data_and_working_directory_are_not_defaults(self):
        step = timeout_step() + "        env:\n          shell: true {0}\n"
        for defaults in (
            "", "defaults:\n  run:\n    working-directory: .\n",
            "env:\n  defaults:\n    run:\n      shell: true {0}\n",
        ):
            with self.subTest(defaults=defaults):
                self.assertEqual(check_step_shell(
                    "public-text", public_text_with(step), step, defaults,
                ), [])


class TestAlertsAcceptanceWiring(unittest.TestCase):
    """Backstop the two banks from the separately invoked public-text suite."""

    COMMANDS = (
        "python3 .github/scripts/test_alerts_staging.py",
        "python3 .github/scripts/test_alerts_namespace_contract.py",
    )

    def workflow(self):
        with open(WORKFLOW_PATH, encoding="utf-8") as handle:
            return handle.read()

    def assert_wiring(self, text):
        jobs = split_jobs(text)
        for job_id in ("tail-projection", "public-text", "ci-ok"):
            self.assertIn(job_id, jobs)
        for job_id in ("tail-projection", "public-text"):
            self.assertIn(job_id, job_needs(jobs["ci-ok"]))
        for command in self.COMMANDS:
            self.assertEqual(check_required_step(
                "tail-projection", jobs["tail-projection"], command, workflow_text=text,
            ), [])
        self.assertEqual(check_required_step(
            "public-text", jobs["public-text"],
            "python3 .github/scripts/test_ci_pnpm_retry.py", workflow_text=text,
        ), [])

    def assert_aggregate(self, text):
        job = split_jobs(text)["ci-ok"]
        self.assertEqual(non_plain_key_lines(job), [])
        self.assertIn(job_level_field(job, "if"), ("always()", "${{ always() }}"))
        self.assertIsNone(job_level_field(job, "continue-on-error"))
        steps = [step for step in split_steps(job) if step_level_field(step, "run") is not None]
        self.assertEqual(len(steps), 1, "Expected one fail-closed aggregate command")
        step = steps[0]
        self.assertIsNone(step_level_field(step, "if"))
        self.assertIsNone(step_level_field(step, "continue-on-error"))
        self.assertEqual(check_step_shell("ci-ok", job, step, text), [])
        lines = step.splitlines()
        blocks = [i for i, line in enumerate(lines)
                  if re.fullmatch(r"(?:      - |        )run: \|[-]?\s*(?:#.*)?", line)]
        self.assertEqual(len(blocks), 1, "Expected a literal aggregate run block")
        body = []
        for line in lines[blocks[0] + 1:]:
            if line.strip() and not line.startswith("          "):
                break
            body.append(line[10:])
        self.assertTrue(any(line.strip() for line in body))
        binding = step_env_field(step, "NEEDS_JSON")
        self.assertEqual(binding, "${{ toJSON(needs) }}")
        needs = job_needs(job)
        self.assertTrue(needs, "Aggregate must depend on the required jobs")
        self.assertEqual(len(needs), len(set(needs)), "Aggregate dependencies must be distinct")
        jq = shutil.which("jq")
        self.assertIsNotNone(jq, "Aggregate fixtures require jq installed on PATH")
        with tempfile.TemporaryDirectory(prefix="ci-aggregate-fixture-") as root:
            # Retain only the resolved prerequisite, not the caller's PATH or secrets.
            binaries = os.path.join(root, "bin")
            os.mkdir(binaries)
            os.symlink(os.path.abspath(jq), os.path.join(binaries, "jq"))
            script = os.path.join(root, "aggregate.sh")
            with open(script, "w", encoding="utf-8") as handle:
                handle.write("\n".join(body) + "\n")
            shell = shlex.split(effective_shell(text, job, step))
            args = [script if arg == "{0}" else arg for arg in shell]
            cases = [(None, "success")] + [
                (job_id, state) for job_id in needs for state in ("failure", "cancelled", "skipped")
            ]
            for job_id, state in cases:
                results = {name: {"result": "success"} for name in needs}
                if job_id is not None:
                    results[job_id]["result"] = state
                resolved = json.dumps(results) if binding == "${{ toJSON(needs) }}" else binding
                result = subprocess.run(
                    args, cwd=root, env={"PATH": binaries + os.pathsep + os.defpath,
                                         "NEEDS_JSON": resolved},
                    capture_output=True, text=True, timeout=5,
                )
                self.assertEqual(result.returncode == 0, state == "success",
                                 f"ci-ok must fail closed on {job_id}={state}: {result.stdout}{result.stderr}")

    def required_alert_step(self, job, command):
        matches = [step for step in split_steps(job) if step_level_field(step, "run") == command]
        self.assertEqual(len(matches), 1, f"Expected one step running {command}")
        return matches[0]

    def test_real_alert_banks_run_in_required_job(self):
        self.assert_wiring(self.workflow())

    def test_real_aggregate_propagates_failed_cancelled_and_skipped_banks(self):
        self.assert_aggregate(self.workflow())

    def test_registration_masks_and_missing_aggregate_dependency_are_rejected(self):
        text = self.workflow()
        job = split_jobs(text)["tail-projection"]
        for command in self.COMMANDS:
            step = self.required_alert_step(job, command)
            for replacement in ("", step.replace(command, ":"),
                                with_step_field(step, "if", "always()"),
                                with_step_field(step, "continue-on-error", "true"),
                                with_step_field(step, "shell", "true {0}")):
                with self.subTest(command=command, replacement=replacement):
                    with self.assertRaises(AssertionError):
                        self.assert_wiring(text.replace(step, replacement, 1))
        aggregate = split_jobs(text)["ci-ok"]
        for job_id in ("tail-projection", "public-text"):
            needs = job_level_field(aggregate, "needs")
            self.assertIsNotNone(needs)
            missing = aggregate.replace(needs, needs.replace(job_id, job_id + "-shadow"), 1)
            with self.subTest(missing=job_id), self.assertRaises(AssertionError):
                self.assert_wiring(text.replace(aggregate, missing, 1))

    def test_aggregate_exit_and_shell_masks_are_rejected(self):
        text = self.workflow()
        aggregate = split_jobs(text)["ci-ok"]
        step = next(step for step in split_steps(aggregate) if step_level_field(step, "run") is not None)
        for replacement in (with_step_field(step, "shell", "true {0}"),
                            with_step_field(step, "continue-on-error", "true"),
                            step.replace("exit 1", "exit 0"),
                            step.replace('.result == "success"', '.result != "pending"')):
            with self.subTest(replacement=replacement), self.assertRaises(AssertionError):
                self.assert_aggregate(text.replace(step, replacement, 1))

    def aggregate_step(self, text):
        return next(step for step in split_steps(split_jobs(text)["ci-ok"])
                    if step_level_field(step, "run") is not None)

    def test_nested_needs_json_cannot_pose_as_the_step_environment_binding(self):
        text = self.workflow()
        step = self.aggregate_step(text)
        binding = "${{ toJSON(needs) }}"
        wrong = step.replace(
            "        env:\n          NEEDS_JSON: " + binding,
            "        with:\n          NEEDS_JSON: " + binding
            + "\n        env:\n          NEEDS_JSON: '{}'",
        )
        self.assertNotEqual(wrong, step)
        with self.assertRaises(AssertionError):
            self.assert_aggregate(text.replace(step, wrong, 1))

    def test_aggregate_cannot_accept_failure_because_another_dependency_succeeded(self):
        text = self.workflow()
        step = self.aggregate_step(text)
        bypass = step.replace(
            'all(.[]; .result == "success")',
            'all(.[]; .result == "success") or .["rust-test"].result == "success"',
        )
        self.assertNotEqual(bypass, step)
        with self.assertRaises(AssertionError):
            self.assert_aggregate(text.replace(step, bypass, 1))

    def test_caller_installed_jq_is_resolved_without_inheriting_the_caller_environment(self):
        jq = shutil.which("jq")
        self.assertIsNotNone(jq, "Aggregate fixtures require jq installed on PATH")
        with tempfile.TemporaryDirectory() as root:
            executable = os.path.join(root, "jq")
            receipt = os.path.join(root, "jq-executed")
            with open(executable, "w", encoding="utf-8") as handle:
                handle.write(
                    "#!/bin/sh\n"
                    'test -z "${CI_AGGREGATE_PRIVATE_CANARY+x}" || exit 91\n'
                    f"printf ran >> {shlex.quote(receipt)}\n"
                    f"exec {shlex.quote(jq)} \"$@\"\n"
                )
            os.chmod(executable, 0o755)
            with patch.dict(os.environ, {"PATH": root + os.pathsep + os.defpath,
                                         "CI_AGGREGATE_PRIVATE_CANARY": "not-for-the-child"}):
                self.assert_aggregate(self.workflow())
            self.assertTrue(os.path.isfile(receipt), "The caller-resolved jq was discarded")

    def test_missing_jq_fails_with_an_actionable_prerequisite(self):
        which = shutil.which
        with patch.object(shutil, "which", side_effect=lambda cmd: None if cmd == "jq" else which(cmd)):
            with self.assertRaisesRegex(AssertionError, "require.*jq.*PATH"):
                self.assert_aggregate(self.workflow())

    def test_container_aggregate_does_not_silently_execute_a_bash_only_body_under_bash(self):
        text = with_defaults_shell(self.workflow(), None, 0)
        original = split_jobs(text)["ci-ok"]
        aggregate = with_defaults_shell(original, None, 4)
        step = next(step for step in split_steps(aggregate) if step_level_field(step, "run") is not None)
        aggregate = aggregate.replace(step, with_step_field(step, "shell", None), 1)
        container = replace_mapping_field(aggregate, "container", "    container: fixture-image", 4,
                                          insert_at=1)
        configured = text.replace(original, container, 1)
        with self.assertRaises(AssertionError):
            self.assert_aggregate(configured)
        step = self.aggregate_step(configured)
        self.assert_aggregate(configured.replace(step, with_step_field(step, "shell", "bash"), 1))

    def test_existing_supported_shell_does_not_break_the_registration_or_aggregate_mutants(self):
        text = self.workflow()
        for job_id, commands in (("tail-projection", self.COMMANDS), ("ci-ok", (None,))):
            for command in commands:
                job = split_jobs(text)[job_id]
                step = self.aggregate_step(text) if command is None else self.required_alert_step(job, command)
                text = text.replace(step, with_step_field(step, "shell", "bash"), 1)
        self.assert_wiring(text)
        self.assert_aggregate(text)
        with patch.object(self, "workflow", return_value=text):
            self.test_registration_masks_and_missing_aggregate_dependency_are_rejected()
            self.test_aggregate_exit_and_shell_masks_are_rejected()

    def test_deleted_bank_is_caught_by_the_surviving_checker_process(self):
        text = self.workflow()
        job = split_jobs(text)["tail-projection"]
        for command in self.COMMANDS:
            step = self.required_alert_step(job, command)
            with self.subTest(command=command), tempfile.TemporaryDirectory() as root:
                scripts = os.path.join(root, ".github", "scripts")
                workflows = os.path.join(root, ".github", "workflows")
                os.makedirs(scripts)
                os.makedirs(workflows)
                checker = os.path.join(scripts, "test_ci_pnpm_retry.py")
                shutil.copyfile(__file__, checker)
                with open(os.path.join(workflows, "ci.yml"), "w", encoding="utf-8") as handle:
                    handle.write(text.replace(step, "", 1))
                # No alert bank is copied or executed: only the surviving checker.
                result = subprocess.run(
                    [sys.executable, checker, "TestAlertsAcceptanceWiring.test_real_alert_banks_run_in_required_job"],
                    capture_output=True, text=True, timeout=30,
                )
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("FAILED (failures=1)", result.stderr)
                self.assertIn(f"no step runs {command}", result.stderr)


if __name__ == "__main__":
    unittest.main()
