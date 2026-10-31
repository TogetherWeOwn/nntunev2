## What changed

-

## Why

-

## How tested

- [ ] `ci-ok` green
- [ ] Manual / staging notes (if applicable):

## Secrets affirmation

- [ ] No secrets/credentials in code, fixtures, logs, or screenshots
- [ ] Fixtures use synthetic creds only (or N/A if no fixtures)

## Staging migration apply

<!-- Required (enforced by `pr-lint`) when this PR adds or modifies a numbered
     file under migrations/; leave it unfilled otherwise. See
     docs/ops.md, "Staging migration apply declaration". `app` migrations need
     the checkbox ticked before merge; `mirror` ones (shared MB cluster) need
     the fields only. -->

- Migration files:
- Target (`app`/`mirror`, per `migrations/manifest.toml`):
- Role:
- Who applies:
- Rollback:
- [ ] Applied to nntunev2_staging and verified (preflight/verify output linked)
