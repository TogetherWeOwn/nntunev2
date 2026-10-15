# Fixture inventory for binding-parity acceptance tests.

Trimmed environments matrix in the same shape as the operations
runbook's matrix: one row per concern, staging and prod resource
names in backticks. All names are manufactured for fixtures.

| Concern / intended consumer | staging | prod |
|---|---|---|
| R2 audio / stream Worker | `nntunev2-audio-staging` | `nntunev2-audio-prod` |
| Ingest Queue | `nntunev2-ingest-staging` | `nntunev2-ingest-prod` |
