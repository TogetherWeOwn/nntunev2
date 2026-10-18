# NNTune v2 Architecture Decision Records

| # | Decision |
|---|---|
| 0001 | [Core transport and SIMD](0001-nntune-core-transport-and-simd.md) |
| 0002 | [Data model and ERD](0002-data-model.md) |
| 0003 | [Cache scope and R2 layout](0003-cache-scope.md) |
| 0004 | [Plan matrix](0004-plan-matrix.md) |
| 0005 | [Billing provider and subscription lifecycle](0005-billing.md) |
| 0006 | [Native REST and Subsonic API surface](0006-api-surface.md) |
| 0007 | [MusicBrainz mirror](0007-mb-mirror.md) |
| 0008 | [Authentication, vault and sessions](0008-auth-vault-sessions.md) |
| 0009 | [Serverless Cloudflare stack](0009-stack.md) |
| 0010 | [Scoped native REST credentials](0010-native-rest-credentials.md) |
| 0011 | [Deployed-build identity separation](0011-deployed-build-identity.md) |

## Numbering and precedence

The merged core ADR retains **0001**. The architecture branch's former
`0001-stack.md` is now **0009**; data/cache/plan/billing/API retain 0002–0006.
This avoids colliding with the already-merged core, mirror (0007) and auth (0008)
records while preserving existing data/cache references. Historical stack
references to “ADR 0001” (including the stack/database context in ADR 0007)
resolve to **0009**; “ADR 0001” for core transport/SIMD means the core record.
New citations must use the linked filename and the unique number above.

Existing implementation is not retroactively rewritten by an ADR. In particular,
ADR 0008 and its applied auth migration remain authoritative for current auth;
ADR 0002 distinguishes that baseline from additive target tables. ADR 0003 pins
environment-qualified art buckets, superseding the shorthand bucket in ADR 0007.

These contracts confirm or adjust parallel work without restarting it. Accepted
design does not claim implementation, production deployment or a review score.
The individual ADRs identify required alignment and acceptance tests; CI,
independent review, security and release gates remain in force.
