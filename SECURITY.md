# Security Policy

## Supported versions

NNTune v2 is pre-release (no published tags or releases). Security fixes
land on `main` (the default branch) and are released
forward; there are no maintained backport branches yet.

## Reporting a vulnerability

**Do not open a public issue.** Use GitHub's
[private vulnerability reporting](../../security/advisories/new) on this repo.

Include: what is affected, steps to reproduce or exploit, and impact. We will
acknowledge within 5 business days and keep you updated until resolution.

## Protections on this repo

- Secret scanning + push protection enabled (repo code-security settings).
- `gitleaks` full-history scan is a required check on every PR
  (`.github/workflows/gitleaks.yml` clones with `fetch-depth: 0` and scans
  `HEAD` history; `gitleaks` is required on the default branch by the
  `protect-main` ruleset alongside `ci-ok`, `pr-lint`, `CodeRabbit` and
  `Devin Review`).
- Dependabot alerts enabled; version-update automation is configured in
  `.github/dependabot.yml` (weekly minor-and-patch groups for Actions,
  cargo, docker, and npm).
