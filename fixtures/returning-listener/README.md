# Returning-listener funnel: synthetic reference only

This is a reproducible contract exercise, not telemetry, a production schema, a
human baseline, or permission to collect data. The database is created in memory
by `test_funnel.py` using fabricated records only. The account class `eligible`
in those tests is a *hypothetical* classification, not a claim that synthetic
accounts are eligible humans. `synthetic` accounts are explicitly excluded.
No credentials, network, persistent database, staging, or production are used.

Run from the repository root with Python 3 and its bundled SQLite:

```sh
python3 -m unittest discover -s fixtures/returning-listener -p 'test_*.py' -v
```

The tests create the tables, insert every labeled synthetic case, execute the
actual `query.sql`, and compare **every output subject and classification** to
independently specified expected values. SQL uses `:as_of`, UTC Unix seconds,
and SQLite window functions; the fixture fixes `as_of` at 10,000,000 seconds.
The synthetic count is the number of `human:` rows classified `counted`;
this is *not* a count of real people.

## Query and reconciliation contract

An eligible, consented account with a nonblank resolved human key belongs to
that hypothetical human. Qualifying first journeys require authenticated
provider setup, account-owned eligible content, advancing decoded audio and a
successful Range seek. Choose the earliest complete journey per human as of
`as_of`, using journey/session IDs to break timestamp ties. A qualifying return
has authenticated advancing audio, was listener-initiated, has a different
nonblank session ID, and completes strictly after that first journey and no
more than seven days later. A count requires its completion in
`(as_of - 30 days, as_of]`. Duplicate rows and multiple eligible accounts for
the same human do not multiply the result. Identity keys and session IDs in
this deliberately tiny model are globally consistent within the fixture; a
real identity/session correlation strategy has **not** been approved.

The output is one row per resolved eligible human plus separate account rows
for excluded or uncertain accounts. `counted` wins when at least one qualifying
return falls in the rolling window. A qualifying return outside it is
`returned_outside_window`; an unreturned first journey is `pending` through
exactly seven days, then `non_return`. No complete first is
`no_qualifying_first`. Agent, synthetic/test, demo, unconsented, unclassified,
and other non-eligible accounts are `excluded`; eligible/consented accounts
with missing identity are `uncertain_identity`, never counted. A return after
`as_of`, same-session playback, an uninitiated return or invalid receipt does
not qualify. This classification is a fixture reconciliation, not a clinical
or product judgment about a real person.

Hand-reconciled expected cases (the tests assert the complete subject map):

| Cases | Expected | Reason |
| --- | --- | --- |
| upper, inside, shared | counted | Upper endpoint and just inside lower endpoint; shared has duplicate journeys and returns across two accounts but one human |
| lower | returned_outside_window | Return exactly at the open lower 30-day boundary |
| too_late, expired | non_return | Return exceeds seven days by one second, or first has expired without return |
| same_time, future, same_session, not_initiated, no_audio, pending_edge | pending | Invalid, future or absent return; pending_edge is exactly seven days |
| incomplete | no_qualifying_first | No successful Range seek |
| agent, synthetic, demo, unclassified, unconsented | excluded | Not consented and/or not an eligible human account |
| unknown | uncertain_identity | Missing human key; not counted |

The three `counted` labels above come from independent test scenarios, not a
combined population or real baseline. The query output intentionally contains
subject-level synthetic rows so reviewers can reconcile each label; it is not
an approved data export format.

## Before any real collection

**Product and Security review is required** before any real collection, join,
or export. Product must agree on consent, eligible account classes, the
first/return receipt definitions, session independence, the human-identity
resolution and reconciliation process; Security must review minimization,
pseudonymization, access controls, secrets exclusion, retention/deletion,
consent and classification handling, and the restricted receipt workflow.
The event schema and retention approvals are **UNKNOWN**, the query has not
been validated against real receipts, and the human baseline is **UNKNOWN / NOT
MEASURABLE**, not zero. No synthetic result establishes demand. Review the bet
by 2026-11-02; if there is no credible participant or demonstrated need, stop
feature expansion and record a stop/reframe decision. This artifact does not
assert that such a decision or either review has occurred.
