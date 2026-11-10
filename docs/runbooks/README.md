# Runbooks

Actual deploy, rollback and recovery procedures with prerequisites,
approval boundaries and safe failure paths. No credentials, internal
addresses or operational bypasses are recorded here. Document
commands; do not perform deployments or drills outside existing
authorization.

- [Deploy and rollback](deploy-rollback.md) — staging promotion
  checklist, exact-head evidence, and the rollback pointer procedure.
- [Operations inventory](../ops.md) — provisioned resources, bindings
  and environment matrix (the detailed ops reference).
- [Database restore runbook](../database-restore-runbook.md) —
  database recovery procedure and its approval gates.
- [Offsite backup runbook](../offsite-backup-runbook.md) — backup
  verification and recovery procedure.
- [Cache policy](../cache-policy.md) and [cost basis](../cost-basis.md)
  — caching behaviour and measured billing context.

Approval boundaries: production promotion requires release approval
through the standard review path plus its gated production lane.
Credential creation or rotation requires explicit owner authorization.
A production promotion that deploys or changes production needs the
Release Manager's approval. Never treat documentation here as
authorization to mutate resources, spend or deploy.
