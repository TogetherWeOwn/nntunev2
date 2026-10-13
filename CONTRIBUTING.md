# Contributing to NNTune v2

## Commits

[Conventional Commits](https://www.conventionalcommits.org/): `type(scope): summary`.

- Types: `feat` `fix` `perf` `refactor` `test` `docs` `build` `ci` `chore` `revert` `style` `security`
- Scope is optional: `feat(search): fuzzy artist matching`.
- Summary is imperative, lowercase-first, <= 100 chars, no trailing period.

## PR title format

The PR title **is** the squash-commit header, so it must be a valid
Conventional Commits header: `type(scope): summary`, <= 100 chars, no
trailing period. The `pr-lint` check enforces this.

No tracker IDs (e.g. `TOG-*`) are required in PR titles or bodies — this is a
public repo. Do not paste internal links or card IDs. The `public-text` CI
job (part of `ci-ok`) enforces this on all tracked files.

## PR body

Follow `.github/pull_request_template.md`: what changed, why, how tested.

## Merge policy

- Squash-merge only; the squash commit title is the PR title.
- Delete the branch on merge (automatic).
- Every PR must pass **`ci-ok` + `gitleaks` + `pr-lint` + Paperclip Review 5/5**
  before merge. These are required status checks on `main`.
- Never commit secrets. The `gitleaks` gate and push protection enforce this.
