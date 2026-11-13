# Delivery plans

This directory holds the approved approach with implemented versus
remaining scope and the exact verification route. Statuses use four
labels: proposed, approved, implemented, verified. A merged change is
not proof of production activation.

- [R1–R4 programme plan](r1-r4-programme.md) — the feature-parity
  programme order (approved 2026-11-03, version 1.4) and its current
  status view.
- [M1 exit specification](../specs/m1-exit.md) — the staging journey
  that gates production launch consideration.

Verification (see [CONTRIBUTING.md](../../CONTRIBUTING.md#verification-github-actions-first)):
full suites and required gates run on GitHub Actions only. Record the
exact head revision and the Actions run and job links for that
revision as evidence. Local execution is limited to bounded
pre-push checks (formatter, linter, type-check) and focused
reproduction of one failing test or crate. Never run a local full
suite, CI mirror or heavy raw-host build.
