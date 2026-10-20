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
import fnmatch
import os
import posixpath
import re
import shlex
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


# Words whose presence makes a RUN's ownership effects unprovable: shells
# reparse quoted data (-c strings, eval arguments) and traps fire after any
# textual grant. rbash is bash's restricted mode (same -c behavior for plain
# external commands); runuser is listed defensively (the runtime stage
# installs util-linux, which ships it).
SHELL_WORDS = frozenset({
    "sh", "bash", "dash", "ash", "ksh", "zsh", "rbash", "su", "runuser",
    "trap", "eval",
})

# Shell metacharacters that tokenize a -c script (or trap/eval argument)
# beyond whitespace: glued forms such as `true;chown`, `(chown ...)` and
# `true&&chown` must expose the inner chown, or resolver-class deliveries
# (`/proc/self/exe -c ...`) that name no interpreter keep dead proof.
INNER_SPLIT_RE = re.compile(r"[\s;|&()<>]+")


def inner_words(word):
    """Split a quoted token the way the RUN shell tokenizes a -c script.

    Quote characters are stripped, never split on: quote removal
    concatenates, so `"chown"`, `'chown'` and `ch""own` are all one word
    `chown` once the -c shell re-parses the script. Without this,
    resolver-class deliveries (`/proc/self/exe -c ...`) smuggle a
    quote-decorated chown past the basename match and keep dead proof.
    """
    cleaned = []
    for piece in INNER_SPLIT_RE.split(word):
        stripped = piece.replace("'", "").replace('"', "")
        if stripped:
            cleaned.append(stripped)
    return cleaned


def word_matches_names(word, names, require_alnum=True):
    """True if `word` could name (or glob-expand to) one of `names`.

    The basename is the pattern and each known name the string, because the
    RUN shell expands the Dockerfile's spelling before exec (`/bin/da?h`
    becomes dash). Bare punctuation with no alphanumerics (notably the `*`
    in `rm -rf /var/lib/apt/lists/*`) never matches in argument position,
    or every cleanup glob would void the proof; in command position there is
    no exemption, since a glob there selects the executed program.
    """
    base = posixpath.basename(word)
    if require_alnum and not any(char.isalnum() for char in base):
        return False
    return any(fnmatch.fnmatchcase(name, base) for name in names)


def concat_candidates(cmd):
    """Each command word plus every run of consecutive words joined bare.

    The outer RUN lexer (posix=False) splits same-quote concatenation such as
    `'cho''wn ...'` into separate words (`cho`, `wn ...`), but the RUN shell
    concatenates adjacent quoted segments into one argv word before the -c
    shell re-parses it. Joined candidates model that concatenation so a
    resolver-class delivery (`/proc/self/exe -c ...`) cannot smuggle a split
    `chown` (or shell name) past the per-word scans.

    The punctuation exemption follows the EXECUTING shell's command position,
    not the outer word index. A whitespace-bearing word is a quoted script the
    inner shell re-tokenizes, so its inner pieces lose the exemption (the
    first is inner command position; later ones may follow `;`, `&&`, parens
    or `exec`). A join starting at cmd[0] that stops before the last word may
    be a quote-joined program name (`'./'` + `*`), so it loses the exemption
    too. Only the full-command join keeps it: its words were separated by
    unquoted whitespace, so they can never be one argv word -- this keeps the
    shipped `rm -rf /var/lib/apt/lists/*` cleanup green.
    """
    for index, word in enumerate(cmd):
        yield word, index > 0 and not any(char.isspace() for char in word)
    last = len(cmd) - 1
    for start in range(len(cmd)):
        joined = ""
        for end in range(start, len(cmd)):
            joined += cmd[end]
            if end > start:
                yield joined, not (start == 0 and end < last)


def scratch_ownership(line, user_name, owned):
    """Apply literal ownership events in order; unsupported syntax clears proof."""
    instr = INSTR_RE.match(line)
    keyword, value = instr.group(1).lower(), instr.group(2)

    def matches_owner(owner):
        return (
            re.fullmatch(PLAIN_USER_RE, owner) is not None
            and owner.partition(":")[0] == user_name
        )

    if keyword in ("copy", "add"):
        # Shell-form COPY/ADD: flags precede sources and the final word is the
        # destination. JSON form, expansions and duplicate --chown fail closed.
        try:
            words = shlex.split(value)
        except ValueError:
            return False
        owners = []
        while words and words[0].startswith("--"):
            flag = words.pop(0)
            if flag.startswith("--chown="):
                owners.append(flag.removeprefix("--chown="))
        if len(words) < 2 or any(c in value for c in "$`[]\\"):
            return False
        if words[-1] not in ("/scratch", "/scratch/"):
            return owned
        return len(owners) == 1 and matches_owner(owners[0])

    # Preserve quotes until AFTER splitting command operators: an echo argument
    # such as "&&" or ";" must never become a command boundary. Only literal
    # simple commands joined by && or ; qualify. Pipelines, || (may skip chown),
    # redirections, substitutions, control flow and exec-form RUN fail closed.
    lexer = shlex.shlex(value, posix=False, punctuation_chars=True)
    lexer.whitespace_split = True
    # shlex otherwise truncates /scratch#x to /scratch, unlike /bin/sh.
    # Unquoted # is outside our plain-token subset, even at a word boundary.
    lexer.commenters = ""
    try:
        tokens = list(lexer)
    except ValueError:
        return False
    commands, command, segment = [], [], 0
    for token in tokens:
        if token in ("&&", ";"):
            if not command:
                return False
            commands.append((segment, command))
            command = []
            if token == ";":
                segment += 1
        elif (
            re.fullmatch(r'''(?:[A-Za-z0-9_./:=+*?@%,-]+|'[^']*'|"[^"]*")''', token)
            and not any(c in token for c in "$`\\")
        ):
            command.append(shlex.split(token)[0] if token else "")
        else:
            return False
    if command:
        commands.append((segment, command))
    elif tokens and tokens[-1] != ";":
        return False
    # These shell words can skip later commands or reinterpret quoted data.
    if any(cmd[0] in {
        "if", "then", "else", "elif", "fi", "for", "while", "until", "do",
        "done", "case", "esac", "in", "exit", "exec", "return", "eval", ".",
    } for _, cmd in commands):
        return False
    # Shell interpreters reparse quoted data; traps may run AFTER a later
    # textual grant. Reject the entire RUN instead of folding those effects
    # in source order. Check every word's basename to catch paths and prefixes
    # (env, command, assignments, busybox), as a glob pattern to catch
    # expansions (`/bin/da?h`), and split quoted tokens on shell metacharacters
    # with quote removal to catch string splits (`env -S 'sh -c ...'`), glued
    # scripts (`true;chown`, `(chown ...)`, `true&&chown`) and quote-decorated
    # names (`"chown"`, `ch""own`). Consecutive words are also scanned joined
    # bare to catch same-quote concatenation (`'cho''wn'`) the outer lexer
    # splits but the RUN shell concatenates. The bare-punctuation exemption
    # follows the executing shell's command position: quoted scripts (the
    # inner shell re-tokenizes them) and cmd[0]-starting partial joins (a
    # quote-joined program name such as `'./'` + `*`) lose it, since a glob
    # there selects the executed program (`./*` after staging). Only the
    # full-command join keeps the exemption, so the `rm -rf .../lists/*`
    # cleanup cannot be one argv word. Even inert mentions fail closed.
    if any(word_matches_names(inner, SHELL_WORDS, require_alnum=require_alnum)
           for _, cmd in commands
           for word, require_alnum in concat_candidates(cmd)
           for inner in inner_words(word)):
        return False
    last_segment = commands[-1][0] if commands else -1
    for segment, cmd in commands:
        # Only the final &&-list must execute AND succeed in a green build.
        # Even its first chown can fail and be rescued by a later ;-list.
        guaranteed = segment == last_segment
        if cmd[0] != "chown":
            # Do not interpret RUN flags, assignments, wrappers or executable
            # paths as harmless text. Only bare chown can establish proof;
            # any other chown-shaped command conservatively invalidates it.
            # Metachar-split, quote-stripped inner words of quoted tokens count
            # too, so deliveries that name no interpreter (`/proc/self/exe -c
            # ...`, including glued scripts, quote-decorated `"chown"` /
            # `ch""own` names and same-quote `'cho''wn'` concatenations
            # rejoined across the outer lexer's word split) still clear proof
            # via the chown they carry.
            # Command position keeps no
            # punctuation exemption: `./*` may expand to a staged chown.
            if any(word_matches_names(inner, {"chown"},
                                      require_alnum=require_alnum)
                   for word, require_alnum in concat_candidates(cmd)
                   for inner in inner_words(word)):
                owned = False
            continue
        args = cmd[1:]
        recursive = bool(args and args[0] in ("-R", "--recursive"))
        if recursive:
            args = args[1:]
        if args and args[0] == "--":
            args = args[1:]
        if len(args) != 2 or re.fullmatch(PLAIN_USER_RE, args[0]) is None:
            # Unsupported chown operands cannot preserve an earlier proof.
            owned = False
            continue
        owner, target = args
        # Relative paths depend on WORKDIR (possibly inherited). Globs may
        # expand to /scratch. Neither can be proved disjoint statically.
        if not target.startswith("/") or any(c in target for c in "*?[]"):
            owned = False
            continue
        canonical = posixpath.normpath("/" + target.lstrip("/"))
        if target in ("/scratch", "/scratch/"):
            if not matches_owner(owner):
                # Even a conditional revocation might run: fail closed.
                owned = False
            elif guaranteed:
                owned = True
        elif canonical == "/scratch" or (recursive and canonical == "/"):
            # Aliases and recursive ancestors may revoke but cannot grant.
            # Disjoint absolute targets such as /tmp preserve earlier proof.
            owned = False
    return owned


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
    # Apply RUN/COPY/ADD in order: later ownership events can revoke a grant.
    # Only the literal subset is modeled, not arbitrary filesystem effects;
    # unsupported syntax clears proof, requiring a subsequent literal grant.
    owned = False
    for line in facts["stage_lines"]:
        owned = scratch_ownership(line, user_name, owned)
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
RUN cat <<EOF
USER root
EOF
RUN chown -R repair:repair /scratch
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

  # Ownership operands and shell command position, not chown-shaped text.
  while IFS=$'\t' read -r name want instruction; do
    printf 'FROM scratch AS runtime\n%s\nUSER repair\n' "$instruction" \
      > "$selftest_tmp/$name.Dockerfile"
    run_case "$name" "$want" "$selftest_tmp/$name.Dockerfile"
  done <<'OWNERSHIP_EOF'
chown-echo-text	1	RUN echo "chown -R repair:repair /scratch"
chown-printf-text	1	RUN printf 'chown repair /scratch' > /entrypoint.sh
chown-wrongpath-then-mention	1	RUN chown -R repair:repair /tmp && ls /scratch
chown-subdir-only	1	RUN mkdir -p /scratch/sub && chown -R repair:repair /scratch/sub
chown-owner-prefix	1	RUN chown -R xrepair:xrepair /scratch
chown-owner-suffix	1	RUN chown -R repairx:repairx /scratch
chown-quoted-and-argument	1	RUN echo "&&" chown repair /scratch
chown-quoted-semicolon-argument	1	RUN echo ";" chown repair /scratch
chown-quoted-command-text	1	RUN echo 'x; chown repair /scratch'
chown-quoted-hash-comment	1	RUN echo "#" # chown repair /scratch
chown-reference-option	1	RUN chown --reference=repair /scratch
chown-reference-in-operand-position	1	RUN chown --reference=repair /tmp /scratch
chown-scratch-suffix	1	RUN chown repair /scratchbery
chown-skipped-or	1	RUN true || chown repair /scratch
chown-control-flow	1	RUN if false; then chown repair /scratch; fi
chown-exit-before-grant	1	RUN exit 0; chown repair /scratch
chown-guard-rescued-true	1	RUN false && chown repair /scratch; true
chown-guard-rescued-echo	1	RUN test -d /nope && chown repair /scratch; echo ok
chown-guard-rescued-cleanup	1	RUN false && chown repair /scratch; rm -rf /var/lib/apt/lists/*
chown-guard-rescued-after-grant	1	RUN true && chown repair /scratch && false; true
chown-guard-final-segment	0	RUN true; mkdir -p /scratch && chown repair /scratch;
chown-unconditional-before-final	1	RUN chown repair /scratch; true && true
chown-failed-grant-rescued-mkdir	1	RUN chown repair /scratch; mkdir -p /scratch
chown-failed-grant-rescued-echo	1	RUN chown repair /scratch; echo ok
chown-failed-chain-rescued	1	RUN chown repair /scratch && true; echo ok
chown-failed-grant-rescued-trailing-semicolon	1	RUN chown repair /scratch; true;
chown-final-chain-followup	0	RUN true && chown repair /scratch && true
chown-revoke-same-line	1	RUN chown repair /scratch; chown root /scratch
chown-revoke-and-chain	1	RUN chown repair /scratch && chown root /scratch
chown-revoke-conditional	1	RUN chown repair /scratch; false && chown root /scratch; true
chown-revoke-then-grant	0	RUN chown root /scratch; chown repair /scratch
chown-revoke-then-conditional-grant	1	RUN chown root /scratch; false && chown repair /scratch; true
chown-revoke-unrelated-path	0	RUN chown root /tmp; chown repair /scratch
chown-revoke-trailing-slash	1	RUN chown repair /scratch; chown -R root:root /scratch/
chown-midword-hash	1	RUN chown repair /scratch#x
chown-midword-hash-composed	1	RUN mkdir -p "/scratch#x" && chown repair /scratch#x
chown-midword-hash-hides-revoke	1	RUN chown repair /scratch#x; chown root /scratch
chown-unsupported-after-grant	1	RUN chown repair /scratch; echo hi # comment
chown-substitution	1	RUN echo "$(echo '; chown repair /scratch')"
chown-missing-owner	1	RUN chown :repair /scratch
chown-no-options	0	RUN chown repair /scratch
chown-trailing-slash	0	RUN chown -R repair:repair /scratch/
chown-quoted-operands	0	RUN chown 'repair:repair' "/scratch/"
chown-semicolon	0	RUN mkdir -p /scratch; chown repair:repair /scratch;
chown-recursive-end-options	0	RUN chown --recursive -- repair:repair /scratch
chown-after-quoted-operator	0	RUN echo "&&" && chown repair:repair /scratch
copy-chown-valid	0	COPY --chown=repair:repair source /scratch
add-chown-valid	0	ADD --chown=repair source /scratch/
copy-chown-quoted-valid	0	COPY --chown="repair:repair" source "/scratch/"
copy-chown-other-owner	1	COPY --chown=xrepair:xrepair source /scratch
copy-chown-subdir	1	COPY --chown=repair:repair source /scratch/sub
copy-chown-source-not-destination	1	COPY --chown=repair:repair /scratch /tmp
copy-chown-mention-not-flag	1	COPY chown-repair-/scratch /tmp
copy-chown-duplicate	1	COPY --chown=repair --chown=root source /scratch
OWNERSHIP_EOF

  # State must carry across mutation instructions, not just a single RUN.
  while IFS=$'\t' read -r name want instruction; do
    printf 'FROM scratch AS runtime\nRUN chown repair /scratch\n%s\nUSER repair\n' "$instruction" \
      > "$selftest_tmp/$name.Dockerfile"
    run_case "$name" "$want" "$selftest_tmp/$name.Dockerfile"
  done <<'ORDER_EOF'
chown-revoke-later-run	1	RUN chown root /scratch
chown-revoke-later-conditional-run	1	RUN false && chown root /scratch; true
chown-revoke-later-copy	1	COPY --chown=root source /scratch
chown-revoke-later-add	1	ADD --chown=root source /scratch/
chown-regrant-later-run	0	RUN chown root /scratch; chown repair /scratch
chown-unsupported-later-run	1	RUN echo hi#x; chown root /scratch
chown-unrelated-later-copy	0	COPY --from=builder source /usr/local/bin/tool
chown-unrelated-later-run	0	RUN chown root /tmp
chown-revoke-dot-alias	1	RUN chown root /scratch/.
chown-revoke-double-leading-slash	1	RUN chown root //scratch
chown-revoke-double-trailing-slash	1	RUN chown root /scratch//
chown-revoke-dotdot-alias	1	RUN chown root /tmp/../scratch
chown-revoke-glob	1	RUN chown root /scrat*
chown-revoke-question-glob	1	RUN chown root /scratc?
chown-revoke-recursive-root	1	RUN chown -R root /
chown-revoke-recursive-root-alias	1	RUN chown --recursive root /tmp/..
chown-revoke-network-flag	1	RUN --network=none chown root /scratch
chown-revoke-mount-flag	1	RUN --mount=type=cache,target=/tmp chown root /scratch
chown-revoke-assignment-prefix	1	RUN FOO=bar chown root /scratch
chown-revoke-absolute-command	1	RUN /usr/bin/chown root /scratch
chown-revoke-env-wrapper	1	RUN env chown root /scratch
chown-revoke-command-wrapper	1	RUN command chown root /scratch
chown-revoke-composed-prefix	1	RUN FOO=bar env /usr/bin/chown root /scratch
chown-revoke-conditional-alias	1	RUN false && chown root /scratch/.; true
chown-revoke-prefixed-same-run	1	RUN chown repair /scratch && env chown root /scratch
chown-unrelated-recursive-run	0	RUN chown -R root /tmp
chown-unrelated-normalized-run	0	RUN chown root //tmp/./
chown-unrelated-prefix-collision	0	RUN chown -R root /scrat
chown-alias-not-grant	1	RUN chown root /scratch; chown repair /scratch/.
chown-wrapper-not-grant	1	RUN chown root /scratch; env chown repair /scratch
chown-regrant-after-wrapper	0	RUN env chown root /scratch; chown repair /scratch
chown-regrant-after-alias	0	RUN chown root /scratch/.; chown repair /scratch
chown-revoke-shell-single-quoted	1	RUN sh -c 'chown root /scratch'
chown-revoke-shell-double-quoted	1	RUN sh -c "chown root /scratch"
chown-revoke-bash-script	1	RUN bash -c 'chown root /scratch'
chown-revoke-dash-script	1	RUN dash -c 'chown root /scratch'
chown-revoke-su-script	1	RUN su -c 'chown root /scratch'
chown-revoke-shell-same-run	1	RUN chown repair /scratch && sh -c 'chown root /scratch'
chown-revoke-shell-path	1	RUN /bin/sh -c 'chown root /scratch'
chown-revoke-shell-env-prefix	1	RUN env /bin/sh -c 'chown root /scratch'
chown-revoke-shell-command-prefix	1	RUN command sh -c 'chown root /scratch'
chown-revoke-shell-assignment-prefix	1	RUN FOO=bar sh -c 'chown root /scratch'
chown-revoke-shell-busybox-prefix	1	RUN busybox sh -c 'chown root /scratch'
chown-revoke-shell-quoted-name	1	RUN 'sh' -c 'chown root /scratch'
chown-revoke-shell-adjacent-semicolon	1	RUN sh -c 'true;chown root /scratch'
chown-revoke-shell-parenthesized	1	RUN sh -c '(chown root /scratch)'
chown-revoke-shell-regrant-same-run	1	RUN sh -c 'chown root /scratch'; chown repair /scratch
chown-revoke-trap-after-grant	1	RUN chown repair /scratch && trap 'chown root /scratch' EXIT
chown-revoke-trap-before-grant	1	RUN trap 'chown root /scratch' EXIT; chown repair /scratch
chown-revoke-trap-before-grant-chain	1	RUN trap "chown root /scratch" 0 && chown repair /scratch
chown-revoke-trap-later-run	1	RUN trap 'chown root /scratch' EXIT
chown-revoke-trap-adjacent-semicolon	1	RUN trap 'true;chown root /scratch' EXIT; chown repair /scratch
chown-revoke-trap-command-prefix	1	RUN command trap 'chown root /scratch' EXIT; chown repair /scratch
chown-revoke-eval-command-prefix	1	RUN command eval 'chown root /scratch'
chown-revoke-nested-shell	1	RUN sh -c "trap 'chown root /scratch' EXIT"
chown-revoke-unsupported-ash	1	RUN ash -c 'chown root /scratch'
chown-revoke-unsupported-ksh	1	RUN ksh -c 'chown root /scratch'
chown-revoke-unsupported-zsh	1	RUN zsh -c 'chown root /scratch'
chown-revoke-shell-glob-dash	1	RUN /bin/da?h -c 'chown root /scratch'
chown-revoke-shell-glob-bash	1	RUN /bin/ba?h -c 'chown root /scratch'
chown-revoke-shell-rbash	1	RUN rbash -c 'chown root /scratch'
chown-revoke-shell-env-split	1	RUN env -S 'sh -c "chown root /scratch"'
chown-revoke-shell-proc-self	1	RUN /proc/self/exe -c 'chown root /scratch'
chown-revoke-chown-glob-command	1	RUN /usr/bin/cho?n root /scratch
chown-revoke-procself-glued-semicolon	1	RUN /proc/self/exe -c 'true;chown root /scratch'
chown-revoke-procself-glued-assign	1	RUN /proc/self/exe -c 'x=1;chown root /scratch'
chown-revoke-procself-glued-paren	1	RUN /proc/self/exe -c '(chown root /scratch)'
chown-revoke-procself-glued-and	1	RUN /proc/self/exe -c 'true&&chown root /scratch'
chown-revoke-procself-glued-pipe	1	RUN /proc/self/exe -c 'true|chown root /scratch'
chown-revoke-procself-glued-redirect	1	RUN /proc/self/exe -c 'true> /tmp/x;chown root /scratch'
chown-revoke-procself-quoted-double	1	RUN /proc/self/exe -c '"chown" root /scratch'
chown-revoke-procself-quoted-single	1	RUN /proc/self/exe -c "'chown' root /scratch"
chown-revoke-procself-quoted-split	1	RUN /proc/self/exe -c 'ch""own root /scratch'
chown-revoke-procself-quoted-path	1	RUN /proc/self/exe -c '"/usr/bin/chown" root /scratch'
chown-revoke-procself-quoted-glued	1	RUN /proc/self/exe -c 'true;"chown" root /scratch'
chown-revoke-procself-quoted-exec	1	RUN /proc/self/exe -c 'exec "chown" root /scratch'
chown-revoke-procself-quoted-shell	1	RUN /proc/self/exe -c '"sh" -c "chown root /scratch"'
chown-revoke-procself-concat-single	1	RUN /proc/self/exe -c 'cho''wn root /scratch'
chown-revoke-procself-concat-double	1	RUN /proc/self/exe -c "cho""wn root /scratch"
chown-revoke-procself-concat-split-start	1	RUN /proc/self/exe -c 'c''hown root /scratch'
chown-revoke-procself-concat-split-end	1	RUN /proc/self/exe -c 'chow''n root /scratch'
chown-revoke-procself-concat-triple	1	RUN /proc/self/exe -c 'c''ho''wn root /scratch'
chown-revoke-procself-concat-glued	1	RUN /proc/self/exe -c 'true;cho''wn root /scratch'
chown-revoke-shell-concat-split	1	RUN 's''h' -c 'cho''wn root /scratch'
chown-revoke-concat-punct-glob	1	RUN './'* root /scratch
chown-revoke-concat-punct-glob-double	1	RUN "./"* root /scratch
chown-revoke-procself-inner-punct-glob	1	RUN /proc/self/exe -c './* root /scratch'
chown-revoke-procself-inner-punct-glob-qmark	1	RUN /proc/self/exe -c './????? root /scratch'
chown-revoke-procself-inner-punct-glob-exec	1	RUN /proc/self/exe -c 'exec ./* root /scratch'
chown-revoke-procself-inner-punct-glob-chain	1	RUN /proc/self/exe -c 'true;./* root /scratch'
chown-revoke-procself-inner-punct-glob-paren	1	RUN /proc/self/exe -c '(./* root /scratch)'
ORDER_EOF

  fixture chown-revoke-workdir-dot.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS runtime
RUN chown repair /scratch
WORKDIR /scratch
RUN chown root .
USER repair
FIXTURE_EOF
  run_case "chown-revoke-workdir-dot" 1 "$selftest_tmp/chown-revoke-workdir-dot.Dockerfile"

  fixture chown-revoke-workdir-relative.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS runtime
RUN chown repair /scratch
WORKDIR /
RUN chown root scratch
USER repair
FIXTURE_EOF
  run_case "chown-revoke-workdir-relative" 1 "$selftest_tmp/chown-revoke-workdir-relative.Dockerfile"

  fixture chown-revoke-punct-glob-command.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS runtime
RUN chown repair /scratch
WORKDIR /x
COPY --from=server /usr/bin/chown /x/a
RUN ./* root /scratch
USER repair
FIXTURE_EOF
  run_case "chown-revoke-punct-glob-command" 1 "$selftest_tmp/chown-revoke-punct-glob-command.Dockerfile"

  fixture chown-revoke-concat-punct-glob-staged.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS runtime
RUN chown repair /scratch
WORKDIR /x
COPY --from=server /usr/bin/chown /x/chown
RUN './'* root /scratch
USER repair
FIXTURE_EOF
  run_case "chown-revoke-concat-punct-glob-staged" 1 "$selftest_tmp/chown-revoke-concat-punct-glob-staged.Dockerfile"

  fixture revoke-then-grant-later-run.Dockerfile <<'FIXTURE_EOF'
FROM scratch AS runtime
RUN chown repair /scratch
RUN chown root /scratch
RUN true && chown repair /scratch
USER repair
FIXTURE_EOF
  run_case "revoke-then-grant-later-run" 0 "$selftest_tmp/revoke-then-grant-later-run.Dockerfile"

  # Unsupported RUNs clear proof, but cannot install traps into a later RUN.
  while IFS=$'\t' read -r name instruction; do
    printf 'FROM scratch AS runtime\n%s\nRUN chown repair /scratch\nUSER repair\n' "$instruction" \
      > "$selftest_tmp/$name.Dockerfile"
    run_case "$name" 0 "$selftest_tmp/$name.Dockerfile"
  done <<'RESTORE_EOF'
chown-regrant-next-run-after-shell	RUN sh -c 'chown root /scratch'
chown-regrant-next-run-after-trap	RUN trap 'chown root /scratch' EXIT
RESTORE_EOF

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
