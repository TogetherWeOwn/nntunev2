#!/usr/bin/env bash
# Pin: the repair runtime stage must run as a non-root user.
#
# The repair server only stages inputs, writes repaired outputs under
# /scratch (REPAIR_SCRATCH_DIR), and serves the repair Worker over HTTP, so
# the runtime stage must end (after its last RUN/COPY/ADD) with a non-root
# USER that owns /scratch. Fails if the runtime stage has no USER after its
# last mutation instruction, if that USER names root (including numeric
# spellings of UID 0 such as 00 or -0, which the runtime resolves the way
# strconv.Atoi does), if any FROM follows the runtime stage (CI builds
# without --target, so the shipped image is the LAST stage), or if no
# chown/--chown grants the user /scratch (the server stages inputs and
# outputs under /scratch).
#
# The scan mirrors the production Dockerfile parser (moby/buildkit) by
# reusing the line model from .github/scripts/check_dependabot_coverage.py:
# an instruction is a backslash-joined logical line, keywords match
# case-insensitively, and RUN/COPY/ADD heredoc bodies are content, never
# instructions -- so a USER hidden in a continuation line or a heredoc body,
# or a lowercase `user root` after the real USER, cannot fool the gate.
# A non-default `escape` parser directive (backtick continuations) would
# invalidate the backslash-only line model, so the pin fails closed when
# the directive prelude sets one.
# The coupling is intentional and fail-closed: if the shared module cannot
# be imported, the pin errors instead of passing.
#
# Run in CI (repair-container job). Optional argv: path to the Dockerfile
# under test (defaults to containers/repair/Dockerfile), or --self-test to
# run the built-in mutant suite (also run in CI).
set -uo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SELF="${BASH_SOURCE[0]}"

check_dockerfile() {
  local dockerfile="$1"
  [[ -f "$dockerfile" ]] || { echo "FAIL: Dockerfile not found at $dockerfile" >&2; return 1; }
  command -v python3 >/dev/null 2>&1 || { echo "FAIL: python3 is required to parse $dockerfile" >&2; return 1; }
  PIN_DC_SCRIPTS="$(cd -- "$script_dir/../../../.github/scripts" && pwd)" \
    python3 - "$dockerfile" <<'PYEOF'
import os
import re
import sys

sys.path.insert(0, os.environ["PIN_DC_SCRIPTS"])
try:
    import check_dependabot_coverage as dc
except ImportError as exc:
    print(f"FAIL: cannot load Dockerfile line model: {exc}")
    sys.exit(1)

# Same shape as the shared module's FROM_RE: Unicode-wide leading
# whitespace, ASCII separator set, case-insensitive keyword.
INSTR_RE = re.compile(r"^\s*(RUN|COPY|ADD|USER)[\t\v\f\r ]+(.*)$", re.IGNORECASE)
PLAIN_USER_RE = r"[A-Za-z0-9_.-]+(:[A-Za-z0-9_.-]+)?"
# Mirrors strconv.Atoi's accepted spellings (optional sign, ASCII digits):
# whatever the container runtime would take as a UID is judged numerically.
NUMERIC_RE = re.compile(r"[+-]?[0-9]+")
# Parser-directive line shape (moby/buildkit directives.go): `# name=value`
# after the marker, name case-insensitive, blanks around `=` allowed.
DIRECTIVE_RE = re.compile(r"^([a-zA-Z][a-zA-Z0-9]*)\s*=\s*(.+?)\s*$")
DIRECTIVE_NAMES = frozenset({"syntax", "escape", "check"})


def is_root_part(part):
    """True for `root` and for numeric spellings at/below UID 0."""
    if part == "root":
        return True
    if NUMERIC_RE.fullmatch(part or "") is not None:
        return int(part) <= 0
    return False


def parse_escape_directive(physical):
    """`(escape, error)` from the directive prelude (None = default backslash).

    Mirrors BuildKit: only leading `# name=value` comments count -- the
    first blank line, non-comment line, non-directive comment, or unknown
    name ends the prelude. A repeated directive is a build error.
    """
    seen = set()
    escape = None
    for raw in physical:
        token = raw.lstrip()
        if token == "" or not token.startswith("#"):
            break
        match = DIRECTIVE_RE.match(token[1:].lstrip())
        if match is None:
            break
        name = match.group(1).lower()
        if name not in DIRECTIVE_NAMES:
            break
        if name in seen:
            return escape, f"duplicate '{name}' parser directive"
        seen.add(name)
        if name == "escape":
            escape = match.group(2)
    return escape, None


def scan(dockerfile):
    """Facts about the runtime stage; line model mirrors the FROM guard."""
    with open(dockerfile, "rb") as handle:
        text = handle.read().decode("utf-8")
    if text.startswith("\ufeff"):
        text = text[1:]
    physical = dc._physical_lines(text)
    escape, directive_error = parse_escape_directive(physical)
    pending = []  # (delimiter, chomp_tabs) heredoc bodies being skipped
    seen_runtime = False
    post_from_line = 0
    mutation = 0
    user_line = 0
    user_value = ""
    stage_lines = []
    i = 0
    while i < len(physical):
        if pending:
            name, chomp = pending[0]
            candidate = physical[i].lstrip("\t") if chomp else physical[i]
            if candidate == name:
                pending.pop(0)
            i += 1
            continue
        first = physical[i].lstrip()
        if first.startswith("#") or first == "":
            i += 1
            continue
        lineno = i + 1
        content, continued = dc._strip_continuation(first)
        chunks = [content]
        while continued:
            i += 1
            if i >= len(physical):
                break
            nxt = physical[i]
            if dc._COMMENT_RE.match(nxt) or nxt.strip() == "":
                continue
            content, continued = dc._strip_continuation(nxt)
            chunks.append(content)
        i += 1
        logical = "".join(chunks)
        from_match = dc.FROM_RE.match(logical)
        if from_match is not None:
            if seen_runtime:
                post_from_line = lineno
                break
            rest = from_match.group(1).strip()
            staged = dc.FROM_STAGE_RE.search(rest)
            if staged is not None and staged.group(1).lower() == "runtime":
                seen_runtime = True
            continue
        # Heredocs are tracked in every stage: a body line must never forge
        # an instruction (e.g. a fake `FROM ... AS runtime`) anywhere.
        for name, chomp in dc._heredoc_openers(logical):
            pending.append((name, chomp))
        if not seen_runtime:
            continue
        instr = INSTR_RE.match(logical)
        if instr is not None:
            keyword = instr.group(1).lower()
            if keyword in ("run", "copy", "add"):
                mutation = lineno
                # Only mutation lines feed the ownership gate: only RUN can
                # execute chown and only COPY/ADD take --chown, so an ENV or
                # LABEL merely mentioning ownership must never satisfy it.
                stage_lines.append(logical)
            elif keyword == "user":
                user_line = lineno
                tokens = instr.group(2).strip().split()
                user_value = tokens[0] if tokens else ""
    return {
        "seen_runtime": seen_runtime,
        "post_from_line": post_from_line,
        "mutation": mutation,
        "user_line": user_line,
        "user_value": user_value,
        "stage_lines": stage_lines,
        "escape": escape,
        "directive_error": directive_error,
    }


def main():
    dockerfile = sys.argv[1]
    try:
        facts = scan(dockerfile)
    except (OSError, UnicodeDecodeError) as exc:
        print(f"FAIL: cannot read {dockerfile}: {exc}")
        return 1
    if facts["directive_error"]:
        print(
            f"FAIL: invalid Dockerfile parser directives: "
            f"{facts['directive_error']}"
        )
        return 1
    if facts["escape"] is not None and facts["escape"] != "\\":
        print(
            f"FAIL: Dockerfile sets a non-default escape token "
            f"({facts['escape']!r}); the pin only models backslash continuations"
        )
        return 1
    if not facts["seen_runtime"]:
        print(f"FAIL: no 'AS runtime' stage in {dockerfile}")
        return 1
    if facts["post_from_line"]:
        print(
            f"FAIL: runtime stage is not the final stage "
            f"(FROM at line {facts['post_from_line']} follows it); "
            f"CI builds without --target so the shipped image is the last stage"
        )
        return 1
    user_value = facts["user_value"]
    if not user_value:
        print(
            f"FAIL: runtime stage has no USER "
            f"(last RUN/COPY/ADD at line {facts['mutation']})"
        )
        return 1
    if facts["user_line"] < facts["mutation"]:
        print(
            f"FAIL: runtime stage USER ({user_value}, line {facts['user_line']}) "
            f"is before the last RUN/COPY/ADD (line {facts['mutation']})"
        )
        return 1
    user_name, _, group_name = user_value.partition(":")
    if (
        not user_name
        or is_root_part(user_name)
        or (group_name and is_root_part(group_name))
    ):
        print(
            f"FAIL: runtime stage USER '{user_value}' "
            f"(line {facts['user_line']}) is root"
        )
        return 1
    if re.fullmatch(PLAIN_USER_RE, user_value) is None:
        print(
            f"FAIL: runtime stage USER '{user_value}' "
            f"(line {facts['user_line']}) is not a plain user[:group]"
        )
        return 1
    # The runtime user must own /scratch: the server stages inputs and writes
    # outputs under REPAIR_SCRATCH_DIR=/scratch, so a USER without /scratch
    # ownership breaks repairs at container runtime.
    # Only RUN/COPY/ADD lines are scanned (a `# chown ... /scratch` remark or an
    # ENV merely mentioning ownership never qualifies); inline remarks are
    # stripped and /scratch is anchored with a boundary so /scratchbery-style paths
    # cannot satisfy the gate.
    chown_pat = re.compile(
        "chown.*" + re.escape(user_name) + r"[^\s]*.*/scratch([ \t\v\f\r/\"']|$)"
    )
    owned = any(
        chown_pat.search(line.split("#", 1)[0]) for line in facts["stage_lines"]
    )
    if not owned:
        print(
            f"FAIL: runtime stage grants '{user_name}' no /scratch ownership "
            f"(need chown or --chown covering /scratch)"
        )
        return 1
    print(
        f"ok: runtime stage runs as non-root '{user_value}' "
        f"(USER line {facts['user_line']}, "
        f"after last RUN/COPY/ADD line {facts['mutation']}) with /scratch ownership"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
PYEOF
}

selftest_tmp=""

self_test() {
  selftest_tmp="$(mktemp -d)" || { echo "self-test FAIL: mktemp failed" >&2; return 1; }
  trap '[[ -n "$selftest_tmp" ]] && rm -rf -- "$selftest_tmp"' EXIT
  local total=0 failures=0

  run_case() { # $1=name $2=want_rc $3...=argv for the pin
    local name="$1" want="$2"
    shift 2
    total=$((total + 1))
    local out rc=0
    out="$(bash "$SELF" "$@" 2>&1)" || rc=$?
    if [[ "$rc" == "$want" ]]; then
      echo "self-test PASS: $name (rc $rc)"
    else
      failures=$((failures + 1))
      echo "self-test FAIL: $name (want rc $want, got rc $rc)" >&2
      printf '%s\n' "$out" | sed 's/^/    /' >&2
    fi
  }

  fixture() { # $1=filename; body on stdin
    cat > "$selftest_tmp/$1"
  }

  run_case "real-dockerfile" 0

  fixture minimal-valid.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
RUN chown -R repair:repair /scratch
USER repair
CMD ["x"]
FIXTURE_EOF
  run_case "minimal-valid" 0 "$selftest_tmp/minimal-valid.Dockerfile"

  fixture lowercase-keywords.Dockerfile <<'FIXTURE_EOF'
from scratch as base
from base as runtime
workdir /scratch
run chown -R repair:repair /scratch
user repair
cmd ["x"]
FIXTURE_EOF
  run_case "lowercase-keywords" 0 "$selftest_tmp/lowercase-keywords.Dockerfile"

  fixture mixed-case-stage.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS base
FROM base AS RUNTIME
WORKDIR /scratch
RUN chown -R repair:repair /scratch
USER repair
CMD ["x"]
FIXTURE_EOF
  run_case "mixed-case-stage" 0 "$selftest_tmp/mixed-case-stage.Dockerfile"

  fixture continuation-chown.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
RUN chmod 0755 /scratch \
    && chown -R repair:repair /scratch
USER repair
CMD ["x"]
FIXTURE_EOF
  run_case "continuation-chown" 0 "$selftest_tmp/continuation-chown.Dockerfile"

  fixture benign-heredoc.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
RUN chown -R repair:repair /scratch
RUN cat <<EOF
USER root
EOF
USER repair
CMD ["x"]
FIXTURE_EOF
  run_case "benign-heredoc" 0 "$selftest_tmp/benign-heredoc.Dockerfile"

  {
    cat "$selftest_tmp/minimal-valid.Dockerfile"
    printf 'user root\n'
  } > "$selftest_tmp/lower-user-root.Dockerfile"
  run_case "lower-user-root" 1 "$selftest_tmp/lower-user-root.Dockerfile"

  fixture continuation-fake-user.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
RUN chown -R repair:repair /scratch
RUN printf x \
USER repair
CMD ["x"]
FIXTURE_EOF
  run_case "continuation-fake-user" 1 "$selftest_tmp/continuation-fake-user.Dockerfile"

  fixture heredoc-fake-user.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
RUN chown -R repair:repair /scratch
COPY <<EOF /tmp/x
USER repair
EOF
CMD ["x"]
FIXTURE_EOF
  run_case "heredoc-fake-user" 1 "$selftest_tmp/heredoc-fake-user.Dockerfile"

  fixture heredoc-spaced-fake-user.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
RUN chown -R repair:repair /scratch
RUN cat << EOF
USER repair
EOF
CMD ["x"]
FIXTURE_EOF
  run_case "heredoc-spaced-fake-user" 1 "$selftest_tmp/heredoc-spaced-fake-user.Dockerfile"

  {
    cat "$selftest_tmp/minimal-valid.Dockerfile"
    printf 'FROM runtime AS shipped\nUSER root\n'
  } > "$selftest_tmp/appended-stage.Dockerfile"
  run_case "appended-stage" 1 "$selftest_tmp/appended-stage.Dockerfile"

  {
    cat "$selftest_tmp/minimal-valid.Dockerfile"
    printf 'from runtime as shipped\nuser root\n'
  } > "$selftest_tmp/appended-stage-lower.Dockerfile"
  run_case "appended-stage-lower" 1 "$selftest_tmp/appended-stage-lower.Dockerfile"

  fixture user-00.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
RUN chown -R 1000:1000 /scratch
USER 00
CMD ["x"]
FIXTURE_EOF
  run_case "user-00" 1 "$selftest_tmp/user-00.Dockerfile"

  sed 's/^USER repair$/USER repair:00/' "$selftest_tmp/minimal-valid.Dockerfile" \
    > "$selftest_tmp/user-group-00.Dockerfile"
  run_case "user-group-00" 1 "$selftest_tmp/user-group-00.Dockerfile"

  sed 's/^USER repair$/USER -0/' "$selftest_tmp/minimal-valid.Dockerfile" \
    > "$selftest_tmp/user-neg-zero.Dockerfile"
  run_case "user-neg-zero" 1 "$selftest_tmp/user-neg-zero.Dockerfile"

  sed 's/^USER repair$/USER 0/' "$selftest_tmp/minimal-valid.Dockerfile" \
    > "$selftest_tmp/user-0.Dockerfile"
  run_case "user-0" 1 "$selftest_tmp/user-0.Dockerfile"

  sed 's/^USER repair$/USER 0:0/' "$selftest_tmp/minimal-valid.Dockerfile" \
    > "$selftest_tmp/user-0-0.Dockerfile"
  run_case "user-0-0" 1 "$selftest_tmp/user-0-0.Dockerfile"

  grep -v '^USER ' "$selftest_tmp/minimal-valid.Dockerfile" \
    > "$selftest_tmp/no-user.Dockerfile"
  run_case "no-user" 1 "$selftest_tmp/no-user.Dockerfile"

  fixture user-before-mutation.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
USER repair
RUN chown -R repair:repair /scratch
CMD ["x"]
FIXTURE_EOF
  run_case "user-before-mutation" 1 "$selftest_tmp/user-before-mutation.Dockerfile"

  fixture no-chown.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
RUN echo hi
USER repair
CMD ["x"]
FIXTURE_EOF
  run_case "no-chown" 1 "$selftest_tmp/no-chown.Dockerfile"

  fixture comment-only-chown.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
# chown -R repair:repair /scratch
RUN echo hi
USER repair
CMD ["x"]
FIXTURE_EOF
  run_case "comment-only-chown" 1 "$selftest_tmp/comment-only-chown.Dockerfile"

  fixture inline-comment-chown.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
RUN echo hi # chown -R repair:repair /scratch
USER repair
CMD ["x"]
FIXTURE_EOF
  run_case "inline-comment-chown" 1 "$selftest_tmp/inline-comment-chown.Dockerfile"

  fixture env-fake-chown.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
ENV NOTE="chown -R repair:repair /scratch"
RUN echo hi
USER repair
CMD ["x"]
FIXTURE_EOF
  run_case "env-fake-chown" 1 "$selftest_tmp/env-fake-chown.Dockerfile"

  fixture escape-backslash-valid.Dockerfile <<'FIXTURE_EOF'
# escape=\
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
RUN chown -R repair:repair /scratch
USER repair
CMD ["x"]
FIXTURE_EOF
  run_case "escape-backslash-valid" 0 "$selftest_tmp/escape-backslash-valid.Dockerfile"

  fixture escape-fake-user.Dockerfile <<'FIXTURE_EOF'
# escape=`
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
RUN chown -R repair:repair /scratch
RUN printf x `
USER repair
CMD ["x"]
FIXTURE_EOF
  run_case "escape-fake-user" 1 "$selftest_tmp/escape-fake-user.Dockerfile"

  fixture escape-hidden-root.Dockerfile <<'FIXTURE_EOF'
# escape=`
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
RUN chown -R repair:repair /scratch
USER repair
ENV NOTE hi \
user root
CMD ["x"]
FIXTURE_EOF
  run_case "escape-hidden-root" 1 "$selftest_tmp/escape-hidden-root.Dockerfile"

  fixture escape-spaced-form.Dockerfile <<'FIXTURE_EOF'
# ESCAPE = `
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
RUN chown -R repair:repair /scratch
RUN printf x `
USER repair
CMD ["x"]
FIXTURE_EOF
  run_case "escape-spaced-form" 1 "$selftest_tmp/escape-spaced-form.Dockerfile"

  fixture escape-after-comment-ignored.Dockerfile <<'FIXTURE_EOF'
# regular comment
# escape=`
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
RUN chown -R repair:repair /scratch
USER repair
CMD ["x"]
FIXTURE_EOF
  run_case "escape-after-comment-ignored" 0 "$selftest_tmp/escape-after-comment-ignored.Dockerfile"

  fixture escape-after-blank-ignored.Dockerfile <<'FIXTURE_EOF'

# escape=`
FROM scratch AS base
FROM base AS runtime
WORKDIR /scratch
RUN chown -R repair:repair /scratch
USER repair
CMD ["x"]
FIXTURE_EOF
  run_case "escape-after-blank-ignored" 0 "$selftest_tmp/escape-after-blank-ignored.Dockerfile"

  run_case "missing-dockerfile" 1 "$selftest_tmp/does-not-exist.Dockerfile"

  echo "self-test: $((total - failures))/$total passed"
  [[ "$failures" == 0 ]]
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test
  exit $?
fi

check_dockerfile "${1:-$script_dir/../Dockerfile}"
