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

Use public GitHub references or describe the problem in the PR. Keep
private links, tracker identifiers, handoff paths and audit artifacts out of
public work. The `public-text` CI
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
# Copy both the package and fully qualified harness test name from the failing Actions job.
python3 scripts/cargo_cache.py run -- test -p <failing_package> <fully_qualified_test_name> -- --exact
# Or select the single failing standalone crate:
python3 scripts/cargo_cache.py run -- test --manifest-path <failing_crate_manifest>
```

Replace the placeholders before running. The exact harness name includes
its module path (for example, `transcode::tests::ffmpeg_argv_is_pinned`),
not just the function name. Retain any feature or test-target selection
needed by that failing test. Verify the intended test actually ran; zero
matching tests is not a successful reproduction, even if Cargo exits 0.
Take a standalone crate's manifest path from the failing Actions job too.

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

Reviewers and the task owner use those Actions links as CI evidence. Keep
exact-SHA production approval and admission, staging and deployed-build
identity, migration and rollback evidence, and review and merge gates. Follow
[the staging promotion checklist](docs/ops.md#staging-promotion-checklist)
for release evidence. Explicitly state what was not run.

Post newly available CI results and independent reviews in the PR conversation.
Run `python3 scripts/post_pr_evidence.py PR_NUMBER HEAD_SHA --job success 'JOB_URL' TESTED_SHA`
with the verified conclusion, full GitHub Actions job URL and actual commit
the run tested. Use the synthetic merge SHA when the run tested a PR merge.
Repeat `--job` for each result. Use `--general-review-cleared` or
`--security-review-cleared` to record a later review decision on its own.
The helper checks the current PR head and writes both run and job links to
a SHA-labeled comment. It accepts same-repository job URLs, including the
numeric `?pr=` suffix copied from the PR Checks tab.
Before merging, open the linked runs, verify the current head and base, and
confirm the required checks and the reviewers' decisions at their sources.
GitHub reports body edits and base retargets as the same `edited` activity.
The required `migrations-staging` and `pr-lint` PR triggers target `main`.
Stacked PRs still run `ci-ok`, including its migration gate. Push an updated
PR head after retargeting a stacked PR to `main` so it gets both required
checks. For stacks that already received either check against another base,
verify both runs were created after the retarget and the migration snapshot
records `main` at the new head before merging.
For substantive title, description or Attestation corrections at the same
head, close and reopen the PR to re-run the title, PR standards and migration
checks with the current metadata. For same-head graph evidence, reopening also
refreshes the graph controls in the required `migrations-gate` job. Post ordinary
evidence updates as comments.

### Changed-lines mutation gate

`changed-lines-mutation` checks new single-line boundaries in `scripts/account_export.py`
and `scripts/genre_root_coverage.py` against the tested PR merge's first parent.

The UTC warning window runs for seven days from the first gate commit on
`main`, then new survivors fail `ci-ok`. Baseline and runner failures always
block. Push after retargeting or to refresh a warning verdict once enforcement
starts. Add the standalone context only after passing runs on merged `main`
and two later PRs. Record all three job URLs.

## Pull-request size

Keep each PR to about **400 counted lines** or fewer, counted as the
`pr-size` labels below count them (additions + deletions, excluding
lockfiles, generated files and fixtures). Split larger work into a series of
reviewable PRs. A larger (`size/L` or `size/XL`) PR states either a split
plan or one line in its body:

```text
Size justification: <why this change cannot land in smaller pieces>
```

The limit is enforced: the `pr-size-gate` job in the same workflow is a
required check on `main` (`protect-main` ruleset, context
`pr-size-gate / gate`) and fails PRs that exceed it without exception
evidence. Thresholds are the org values (warn from 200, fail over 400
lines / 20 files, hard stop 1000) and are never raised per repo.

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
and fixture/test data). The labelling job is advisory only: it is not a required check
(not in the `protect-main` required checks) and never blocks a merge
by itself. The gate job (`pr-size-gate`) in the same workflow *is*
required and blocks over-limit PRs. A Monday run of the same workflow
(`17 7 * * 1`) prints the weekly tally of merged pull requests per size; the
labels are the durable record behind it.

## Merge policy

The task thread owns the PR and its follow-through without separate role
handoffs for ordinary PRs. The task owner makes ordinary technical and
staging decisions, makes conservative UI decisions, and describes user-visible
changes in the PR. Follow the risk floors and Attestation format
in the [PR template](.github/pull_request_template.md).
Apply the highest tier required by the change, the PR template or the
[review map](.github/review-tiers.yml). Map low to T2 for eligible docs-only
changes and T1 otherwise, ordinary to T1, and sensitive or unclassified to
T0. Content triggers can raise the tier. Changes to `AGENTS.md`, `CLAUDE.md`,
`CONTRIBUTING.md`, `README.md`, `REVIEW.md`, `ROADMAP.md`, `.claude/**` or
`.github/**` require at least T1 review. When the map classifies the path or
content as sensitive, require T0 even if the Attestation validator permits
a lower tier.

| Review tier | Independent read-only review before merge |
| --- | --- |
| T2, eligible docs-only changes | Task author self-check |
| T1, standard changes | One reviewer from a different model family than the author |
| T0, critical changes | Two independent reviewers from a different model family than the author, one general and one security-focused, in parallel |

In T3 Code, delegate review of the full PR head SHA and its diff. Reviewers
independently verify the required tier and report the SHA, model used and
findings. Resolve or answer the findings before merging. After a push, verify
the new head is equivalent to the reviewed patch or obtain a new independent
review for material changes. Required checks must pass on the final head in
either case. The task owner merges through branch protection with the expected
head SHA once the review and CI gates pass. Reviewers report findings and
coverage without changing the PR or merging it.

- Squash-merge only (repo allows squash only; `protect-main` ruleset allows
  only `squash`, merge queue merges with `SQUASH`); the squash commit title
  is the PR title.
- Delete the branch on merge (automatic repo setting).
- Verify the effective branch rules before merging. Require **`ci-ok`,
  `gitleaks`, `pr-lint`, `pr-size-gate / gate` and `migrations-staging`**
  alongside the required org PR-standards workflow and CodeQL rules.
  Verify **`Devin Review`** separately on the final head, even if GitHub
  already allows the merge. CodeRabbit is advisory. Answer its findings
  when it reviews without waiting for it or triggering it manually.
- `main` currently deploys staging. Recheck automatic deployment paths and
  default-head provider deployments and checks before merging, including for
  docs-only changes. Resolve any uncertainty before merging. If merging
  could deploy to production, stop and handle the change through the separate
  [production release process](#production-releases).
- Fix each bot-review finding, or answer it in the thread with evidence
  from the code. Check a suggested fix before applying it (the disproof
  pass in `REVIEW.md`). Use the first-line reply format from the
  [org contract](https://github.com/TogetherWeOwn/.github/blob/main/CONTRIBUTING.md#4-reviews)
  and resolve threads after answering them.
- Tests prove behaviour. Do not add bespoke oracle or proof scripts just to
  satisfy a reviewer; a new guard or gate script needs its own justification
  in the PR body (what it catches that tests cannot).
- For [staging migrations](docs/ops.md#staging-migrations), applies are
  recorded in `schema_migrations` and the staging deploy gate compares them
  with `migrations/manifest.toml`. Obtain and record independent approval of
  the exact DDL as specified in the runbook before dispatching
  `.github/workflows/migrations-staging-apply.yml` from `main`.
  Supply the approved full commit `ref`, migration `file`, `applied_by` and
  the recorded approval reference required by `board_approval`. If the
  approval or its required evidence is unavailable, stop before dispatch.
  The supplied ref is migration data only. The workflow uses trusted code
  from `main` and the staging-scoped credential. Merge only once a successful
  apply is recorded in the job summary. A PR that renames a migration must
  keep its bytes identical and carry its `renamed_from` manifest history
  (`migrations-staging` enforces it).
- Never commit secrets. The `gitleaks` gate (`.github/workflows/gitleaks.yml`,
  full-history scan) and push protection (secret scanning + push protection
  enabled) enforce this.

## Production releases

A production deploy, migration, secret or grant change, or configuration or
flag change is a separate release task. The release task owner records the
plan and evidence before making the production change.

1. Identify the exact commit and, where applicable, the artifact to release.
   Verify that all required checks passed on that candidate and that each
   included PR received the reviews required by its risk tier.
2. Verify that the same build ran first on staging. Record its deployed-build
   identity and passing health and post-deploy journey checks using the
   [staging promotion checklist](docs/ops.md#staging-promotion-checklist).
3. Resolve any unknowns that could make recovery unsafe. Document the rollback
   target and steps, and rehearse the rollback where the repository supports
   it. Take and verify a backup or snapshot before any production data or schema
   change.
4. Commission a separate cross-family production release review. Obtain the
   reviewer's explicit approval for the exact candidate after they review the
   plan and evidence. Record the reviewer, decision and accepted SHA in the
   release task. If they block the release, address the findings and obtain a
   fresh review. This decision supplies the Release Manager acceptance named in
   existing workflows and runbooks. The release task owner coordinates dispatch
   with the existing credential and environment custodians.
5. Use the existing protected workflow for the action. Meet its exact-SHA
   admission, environment protection, credential custody and approval-reference
   requirements, where applicable. For `deploy-production`, the GitHub
   custodian verifies the recorded approval for the exact commit before
   setting `PRODUCTION_ACCEPTED_SHA` in the `production` Environment only.
   Stop before dispatch if no protected workflow supports the action or if
   the workflow's enablement gates are closed.
6. Watch production health and errors for at least 30 minutes after release.
   Roll back immediately on a regression and record the outcome.

Escalate to the repository owner only for spending beyond approved budgets,
permanent deletion of member data, shutting down a member-facing service,
or genuine legal, terms-of-service or privacy questions.
