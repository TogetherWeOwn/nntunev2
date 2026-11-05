# Contributing to NNTune v2

## Commits

[Conventional Commits](https://www.conventionalcommits.org/): `type(scope): summary`.

- Types: `feat` `fix` `perf` `refactor` `test` `docs` `build` `ci` `chore` `revert` `style` `security`
- Scope is optional: `feat(search): fuzzy artist matching`.
- Summary is imperative, lowercase-first, <= 100 chars, no trailing period.

## PR title format

The PR title **is** the squash-commit header, so it must be a valid
Conventional Commits header: `type(scope): summary`, <= 100 chars, no
trailing period. The `pr-lint` job (`.github/workflows/pr-lint.yml`)
enforces the shape, the 12-type list, the 100-char limit and the
no-trailing-period rule.

No tracker IDs (e.g. `TOG-*`) are required in PR titles or bodies — this is a
public repo. Do not paste internal links or card IDs. The `public-text` CI
job (`.github/workflows/ci.yml`, in `ci-ok`'s `needs`, which fails when any
needed job does not succeed; scan script
`.github/scripts/public_text_lint.py`) enforces this on all tracked file
contents. Scope is file contents only: branch names,
commit messages, and PR text stay clean by contributor discipline (prefer
id-free branch names; squash-merge keeps branch names off main).

## PR body

Follow `.github/pull_request_template.md`: what changed, why, how tested.

## Verification: GitHub Actions first

Full test suites and CI gates run on **GitHub Actions**, not on the shared
host or as a local CI mirror. Push commits to the PR branch and open a
**draft PR while work is incomplete**; branch pushes without a PR do not
trigger this repo's PR CI. Mark it ready only when the work is complete.
The workflow is [`.github/workflows/ci.yml`](.github/workflows/ci.yml)
(`ci`, not `check.yml`); `ci-ok` aggregates its required jobs and fails
closed when a required job fails, is cancelled or unexpectedly skips.
Keep every existing required check, security control and review gate.

### Focused reproduction only

Local test execution is limited to reproducing **one failing test or
crate**, selected from the Actions failure. Command listings in component
READMEs describe coverage, not permission to run every listed command as a
local checklist. Full workspace tests, complete frontend/E2E suites and
CI-mirror scripts belong in Actions, including when a local pool is free.
For documentation-only changes, use focused static/diff checks instead.

For a small Rust reproduction, use the bounded Cargo pool from the repo
root (see [pool rules](AGENTS.md#local-rust-builds-the-bounded-cargo-pool)):

```sh
# Substitute the failing test name from the Actions job; do not run all crates.
python3 scripts/cargo_cache.py run -- test -p nntune-core <failing_test_name> -- --exact
# Or select the single failing standalone crate:
python3 scripts/cargo_cache.py run -- test --manifest-path workers/api/Cargo.toml
```

Heavy focused reproductions/builds use the guarded **`boat` PATH wrapper**,
never raw host builds. Retain its sizing, budget and security controls;
add `--no-env`, use a reviewed source-only export of tracked files with no
credentials or incidental files, and verify box deletion/cleanup on success
or failure. Record the tested SHA and any tracked-input changes. Never
bypass the wrapper, use `--keep`, or forward host secrets.

Boat fallback does **not** authorize a full suite, CI mirror or heavy
raw-host run. A small focused Rust fallback still uses the pool. Pool exit
75 means refused: retry the focused reproduction later or defer to Actions;
a missing pool never permits bare compiling Cargo, a per-worktree `target/`
or a manual `CARGO_TARGET_DIR`. If no permitted focused path is available,
use Actions or route the actual prerequisite instead of bypassing controls.

### Evidence for review and release

Record the PR's full head SHA and **Actions run and job URLs for that exact
head**, with conclusions and any skips. For `pull_request` runs, Actions may
show the synthetic merge commit as the run SHA: record it separately and
verify the associated PR head (check-suite/PR metadata) matches the reviewed
head. A green run for an older head, a local transcript, a successful workflow
with skipped required jobs, or a formatting check is not CI evidence.

Reviewers and the Release Manager accept those Actions links as CI evidence;
they do not require a local full-suite transcript. This changes execution
location only: exact-head production authorization, staging/deployed-build
identity, migrations, rollback evidence and normal review/merge gates remain
mandatory. CI success alone is not staging or production acceptance. Follow
[the staging promotion checklist](docs/ops.md#staging-promotion-checklist)
for release evidence. Explicitly state what was not run.

## Pull-request size

Keep each PR to about **400 counted lines** or fewer, counted as the
`pr-size` labels below count them (additions + deletions, excluding
lockfiles, generated files and fixtures). Split larger work into a series of
reviewable PRs. A larger (`size/L` or `size/XL`) PR states either a split
plan or one line in its body:

```text
Size justification: <why this change cannot land in smaller pieces>
```

The limit is a working agreement, not a gate: the `pr-size` workflow stays
advisory and no required check enforces it.

## Pull-request size labels (advisory)

`.github/workflows/pr-size.yml` labels every pull request `size/S` (up to 100
counted lines), `size/M` (up to 400), `size/L` (up to 1000) or `size/XL`
(thresholds fixed centrally in the org script; the caller keeps no copy),
and keeps one advisory comment on pull requests over the 400-line threshold.
Counted lines are additions + deletions excluding lockfiles, generated files
and fixtures. The labeller lives once in the org
[TogetherWeOwn/.github](https://github.com/TogetherWeOwn/.github) repository
(`CI-STANDARD.md`, "Advisory PR-size labels"); the workflow here is a thin
caller with both `uses:` refs pinned to one central SHA and `exclude-extra:
""` (none today — the central exclusions already cover this repo's lockfiles
and fixture/test data). This is advisory only: it is not a required check
(caller `ADVISORY ONLY` header; absent from the `protect-main` required
checks) and never blocks a merge. A Monday run of the same workflow
(`17 7 * * 1`) prints the weekly tally of merged pull requests per size; the
labels are the durable record behind it.

## Merge policy

- Squash-merge only (repo allows squash only; `protect-main` ruleset allows
  only `squash`, merge queue merges with `SQUASH`); the squash commit title
  is the PR title.
- Delete the branch on merge (automatic repo setting).
- Every PR must pass the required status checks on `main` (the
  `protect-main` ruleset): **`ci-ok`, `gitleaks`, `pr-lint`,
  `migrations-staging` and `Devin Review`**. CodeRabbit is advisory: answer
  its findings when it reviews, but never wait for it or trigger it manually.
- A PR merges once every required check is green and every review finding
  is fixed or answered with evidence. The final-stage reviewer merges the
  pinned, settled head before approving; the author merges only when there
  is no review stage. `main` currently deploys staging, not production.
  Recheck both automatic deployment paths and default-head provider
  deployments/checks before classifying a merge as non-production. A merge
  that reaches production, including docs-only changes or an uncertain
  deployment path, needs the Release Manager's exact-head authorization.
  Manual production promotion through `deploy-production` separately needs
  that authorization; merging staging-only changes does not authorize it.
- Fix each bot-review finding, or answer it in the thread with evidence
  from the code. Check a suggested fix before applying it (the disproof
  pass in `REVIEW.md`).
- Tests prove behaviour. Do not add bespoke oracle or proof scripts just to
  satisfy a reviewer; a new guard or gate script needs its own justification
  in the PR body (what it catches that tests cannot).
- Staging migrations: staging is verified against the database itself —
  applies are recorded in `schema_migrations` and the staging deploy gate
  compares them with `migrations/manifest.toml`; see "Staging migrations"
  in `docs/ops.md`. Green checks are not enough for such a PR: the DDL
  needs board approval before it is applied to staging (via
  `.github/scripts/apply_migration.py`), and the PR merges only once the
  apply is recorded. A PR that renames a migration must rename it
  byte-identically and carry its `renamed_from` manifest history
  (`migrations-staging` enforces it).
- Never commit secrets. The `gitleaks` gate (`.github/workflows/gitleaks.yml`,
  full-history scan) and push protection (secret scanning + push protection
  enabled) enforce this.
