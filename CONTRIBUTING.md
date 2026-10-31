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
  `migrations-staging`, `CodeRabbit` and `Devin Review`**.
- A PR merges without owner approval once every required check is green and
  every review finding is fixed or answered with evidence. The author (the
  agent or person who did the work) merges it. `main` deploys staging, not
  production, so no owner approval applies to merging here. Promoting to
  production (the gated `deploy-production` lane) still needs the owner's
  approval.
- Bot review (CodeRabbit, Devin Review) is part of the gate. Fix each
  finding, or answer it in the thread with evidence from the code. Check a
  suggested fix before applying it (the disproof pass in `REVIEW.md`).
- Tests prove behaviour. Do not add bespoke oracle or proof scripts just to
  satisfy a reviewer; a new guard or gate script needs its own justification
  in the PR body (what it catches that tests cannot).
- Staging migrations: a PR that adds or changes `migrations/*.sql` needs a
  `Staging migration:` line in its body (`migrations-staging` enforces it);
  see "Staging migrations" in `docs/ops.md`.
- Never commit secrets. The `gitleaks` gate (`.github/workflows/gitleaks.yml`,
  full-history scan) and push protection (secret scanning + push protection
  enabled) enforce this.
