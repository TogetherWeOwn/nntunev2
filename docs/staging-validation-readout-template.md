# Staging validation readout — evidence template

The staging validation readout is the one-page, evidence-based summary used
for the launch go/no-go decision after the synthetic staging journey has run.
This file fixes the readout's shape **before** any real run, so evidence is
collected uniformly: every section, the evidence-link format, the cost-basis
table, and the kill-rule verdict format are defined here.

This template is not a readout. It records no staging result, no real cost
figure, and no launch decision. The filled example at the bottom uses
synthetic sample data only and must never be cited as a real run.

## How to use

1. Copy the [blank readout](#blank-readout-copy-from-here) into a new file
   named `docs/readouts/<YYYY-MM-DD>-staging-validation.md`.
2. **Before** the validation run, fill in the header (exact build SHA,
   environment, date window) and pre-register every kill rule with its
   threshold. A threshold written after seeing the results is not a kill
   rule; record it as an observation instead.
3. Run the synthetic staging flow and fill each row from evidence, not from
   memory. Every claim needs an evidence entry (`E<n>`); a claim without one
   is deleted or marked `INCONCLUSIVE`.
4. QA verifies that every evidence link resolves and proves the claim next
   to it, then signs section 7. The decision owner records the go/no-go and
   kill-rule verdicts in section 6. Neither role signs the other's section.
5. Keep it to one page of substance (tables excluded). Detail lives behind
   the evidence links, not in prose.

## What a readout must never contain

- **Personal data:** real names, email addresses, account IDs, IP addresses,
  provider (BYOP) usernames, or screenshots/traces showing any of these.
  Refer to people by role only (for example "QA", "decision owner").
  Synthetic accounts must be labelled synthetic.
- **Secret values:** tokens, passwords, database URLs, Access client
  secrets, API keys, or signed URLs embedding any of these. Secret *names*
  (for example `CF_ACCESS_CLIENT_SECRET`, value never shown) are allowed
  only to state that authorized injection was verified.
- **Unsupported demand claims:** user counts, conversion, willingness to
  pay, market size, or "interest" figures without a cited source, sample
  size, and collection method in the evidence index. Staging evidence can
  prove the journey works; it cannot prove demand.
- **Production facts presented as staging facts** (or the reverse):
  extrapolated production cost or load must be labelled an estimate with
  its formula, never "measured".
- **Real content identifiers:** real NZB names, release names, or library
  contents. The synthetic flow uses checked-in synthetic fixtures only.

If any of the above is found in a draft readout, the readout is withdrawn,
redacted, and re-reviewed before any verdict is recorded.

## Evidence-link format

Every row in sections 1, 2, and 6 of the readout cites one or more entries
from the evidence index in section 5 (price sources in section 3 carry
their own dated links inline). Each index entry has exactly these fields:

| Field | Rule |
|---|---|
| ID | `E1`, `E2`, … in order of first citation. Stable once published; never renumber. |
| Claim proven | One observable sentence, for example "staging `/api/health` returned 200 on the promoted build". |
| Build + environment | Exact commit SHA (full or ≥12 chars) and the environment name. Evidence from a different SHA does not prove a claim about this one. |
| Link | Permalink to the specific run, artifact, or record (CI run, E2E report, log query, pricing page with access date) — never a dashboard home page, a "latest" alias, or a URL carrying credentials. In-repo evidence uses a repo-relative path plus line range. |
| Observed | The `YYYY-MM-DD` the evidence was produced. |
| Result | `PASS`, `FAIL`, or `SKIPPED`. `SKIPPED` never counts as a pass; a skipped must-pass step makes its row `INCONCLUSIVE`. |
| Verified by | Role + date of the person who opened the link and confirmed it proves the claim. The author of a row does not verify their own evidence. |

Same-build rule: if the deployed build identity cannot be compared with the
promoted SHA (for example a health endpoint that returns no build identity),
the affected rows are `INCONCLUSIVE` and the readout says so explicitly —
deploy-run success alone is not same-build evidence (see the staging
promotion checklist in `docs/ops.md`).

## Kill-rule verdict format

Kill rules are pre-registered, measurable stop conditions. Each one is a
single row:

| Field | Rule |
|---|---|
| ID | `K1`, `K2`, … |
| Condition | One measurable condition, stated before the run: metric, comparator, threshold, and measurement window. |
| Source of threshold | Where the number came from (budget, SLO, policy) — a link or a one-line rationale. |
| Observed value | The measured value from this run, with its evidence ID. |
| Verdict | `TRIGGERED`, `NOT TRIGGERED`, or `INCONCLUSIVE` (evidence missing or ambiguous). |

Overall verdict line, exactly one, recorded by the decision owner:

```text
VERDICT: GO | NO-GO | KILL — <one sentence naming the deciding evidence IDs> (<role>, <YYYY-MM-DD>)
```

- Any `K<n>` `TRIGGERED` ⇒ overall `KILL`. The readout stops there; it does
  not argue around its own kill rule.
- No kill rule triggered, but any must-pass row `FAIL` or `INCONCLUSIVE`
  ⇒ `NO-GO` until a new readout on a new build resolves it.
- `GO` requires every must-pass row `PASS`, every kill rule
  `NOT TRIGGERED`, and QA sign-off on the evidence index.
- `GO` in a staging readout means "eligible for the launch decision". It is
  not production activation and authorizes no production action by itself.

---

## Blank readout (copy from here)

# Staging validation readout — <YYYY-MM-DD>

- Build: `<full commit SHA>` on `main` (promoted <YYYY-MM-DD>)
- Environment: staging (hostname and bindings per `docs/ops.md`)
- Run window: <YYYY-MM-DD> to <YYYY-MM-DD>
- Authors: <roles only> · QA verifier: <role> · Decision owner: <role>
- Pre-registered kill rules: section 6 of this document, frozen at revision
  `<commit or timestamp before the run>`

### 1. Synthetic staging flow

Synthetic accounts and checked-in synthetic fixtures only. Mark each row
`PASS`, `FAIL`, or `INCONCLUSIVE` (includes `SKIPPED` evidence) with its
evidence IDs. Rows marked † are must-pass for a `GO`.

| # | Step (user-observable) | Expected outcome | Result | Evidence |
|---|---|---|---|---|
| 1† | Access gate | Unauthenticated request to staging is challenged; no bypass used during the run | | |
| 2† | Signup | Synthetic user can create an account and sign in | | |
| 3† | BYOP configuration | Synthetic provider credentials are accepted, stored encrypted, and never appear in logs or UI after entry | | |
| 4† | Ingest | Synthetic fixture (`e2e/fixtures/synthetic.nzb` or successor) ingests to a playable library item | | |
| 5† | Play / seek | The ingested item plays and seeks in the web player | | |
| 6 | Metering | Playback/ingest usage is metered against the synthetic account | | |
| 7† | Rollback pointer | Last-good staging SHA and its push-triggered `deploy.yml` run with `staging-web` conclusion `success` (not `skipped`) are recorded before promotion; Workers rollback evidence is recorded separately or listed as not verified | | |

### 2. User-visible failures

Every failure a user could see during the run, in the user's terms. Empty
table plus the sentence "no user-visible failures observed" is a valid
entry; an omitted table is not.

| # | Flow step | What the user sees | Severity (`blocks journey` / `degrades` / `cosmetic`) | Evidence | Disposition (`fix before GO` / `accepted — follow-up link`) |
|---|---|---|---|---|---|
| | | | | | |

### 3. Infrastructure cost basis

Unit prices are public list prices captured on the `Price source` date;
usage volumes are either measured on staging (say so) or stated assumptions
(labelled `assumed`). One row per metered resource the staging journey
touches. Fixed platform fees go in their own rows so the variable
cost-as-function-of-usage stays visible.

| Resource | Unit | Unit price | Price source (link + date) | Usage basis (`measured` / `assumed`, volume) | Formula | Monthly estimate |
|---|---|---:|---|---|---|---:|
| Workers requests + CPU | | | | | | |
| R2 storage + operations | | | | | | |
| KV reads/writes/storage | | | | | | |
| Queues operations | | | | | | |
| Analytics Engine events | | | | | | |
| Postgres (app + mirror clusters) | | | | | | |
| Egress / other metered items | | | | | | |
| **Total at stated usage** | | | | | | |

State below the table: what usage the total assumes, which line dominates as
usage grows, and what was **not** metered. Cost-recovery arithmetic belongs
to the decision record, not here; this table supplies its input only.

### 4. Explicit non-goals

Copy the standing non-goals and add run-specific ones. A readout that omits
this section is incomplete.

- No production activation, production migration, or production data.
- No real users, real provider accounts, or real content.
- No demand, pricing, or willingness-to-pay conclusion (see "must never
  contain").
- No load, soak, or multi-region claim beyond the single synthetic journey
  actually run.
- <run-specific non-goals>

### 5. Evidence index

| ID | Claim proven | Build + environment | Link | Observed | Result | Verified by |
|---|---|---|---|---|---|---|
| E1 | | | | | | |

### 6. Kill rules and verdict

| ID | Condition (pre-registered) | Source of threshold | Observed value | Evidence | Verdict |
|---|---|---|---|---|---|
| K1 | | | | | |

```text
VERDICT: <GO | NO-GO | KILL> — <one sentence naming the deciding evidence IDs> (<role>, <YYYY-MM-DD>)
```

### 7. Sign-off

- QA: every evidence link in section 5 was opened and proves its claim;
  `SKIPPED` results are not counted as passes. `<role>, <YYYY-MM-DD>`
- Decision owner: verdict above recorded; non-goals in section 4 stand.
  `<role>, <YYYY-MM-DD>`
- What could not be verified: <explicit list, or "nothing">

---

## Filled synthetic example (trial fill — not a real run)

> **SYNTHETIC EXAMPLE.** Every SHA, link, date, price, and result below is
> invented sample data showing the format only. It is not evidence that any
> staging run happened, and it must never be cited as one. Links use
> `example.invalid` placeholders so they cannot be mistaken for real runs.

# Staging validation readout — 2026-01-15 (synthetic example)

- Build: `0123456789abcdef0123456789abcdef01234567` on `main` (promoted 2026-01-14)
- Environment: staging (hostname and bindings per `docs/ops.md`)
- Run window: 2026-01-14 to 2026-01-15
- Authors: release engineer · QA verifier: QA · Decision owner: product lead
- Pre-registered kill rules: section 6 of this document, frozen at revision
  `2026-01-13T00:00:00Z`, before the run

### 1. Synthetic staging flow

| # | Step (user-observable) | Expected outcome | Result | Evidence |
|---|---|---|---|---|
| 1† | Access gate | Unauthenticated request to staging is challenged; no bypass used during the run | PASS | E1, E2, E3 |
| 2† | Signup | Synthetic user can create an account and sign in | PASS | E1, E4 |
| 3† | BYOP configuration | Synthetic provider credentials are accepted, stored encrypted, and never appear in logs or UI after entry | PASS | E1, E5, E6, E7, E8 |
| 4† | Ingest | Synthetic fixture ingests to a playable library item | PASS | E1, E9 |
| 5† | Play / seek | The ingested item plays and seeks in the web player | FAIL | E1, E10 |
| 6 | Metering | Playback/ingest usage is metered against the synthetic account | INCONCLUSIVE | E1, E11 |
| 7† | Rollback pointer | Last-good staging SHA and its push-triggered `deploy.yml` run with `staging-web` conclusion `success` (not `skipped`) are recorded before promotion; Workers rollback evidence is recorded separately or listed as not verified | PASS | E12, E13 |

### 2. User-visible failures

| # | Flow step | What the user sees | Severity | Evidence | Disposition |
|---|---|---|---|---|---|
| 1 | Play / seek | Player spins indefinitely after seeking past the buffered range; no error message | blocks journey | E10 | fix before GO |
| 2 | Signup | Confirmation screen shows a raw message key instead of text on one locale | cosmetic | E14 | accepted — follow-up link in synthetic tracker (placeholder) |

### 3. Infrastructure cost basis

All prices and volumes invented for format demonstration.

| Resource | Unit | Unit price | Price source (link + date) | Usage basis | Formula | Monthly estimate |
|---|---|---:|---|---|---|---:|
| Workers requests + CPU | 1M requests | $0.30 | <https://example.invalid/pricing-workers> (2026-01-01) | assumed, 2M requests | 2 × $0.30 | $0.60 |
| R2 storage + operations | GB-month | $0.015 | <https://example.invalid/pricing-r2> (2026-01-01) | measured on staging, 40 GB | 40 × $0.015 | $0.60 |
| KV reads/writes/storage | 1M reads | $0.50 | <https://example.invalid/pricing-kv> (2026-01-01) | assumed, 1M reads | 1 × $0.50 | $0.50 |
| Queues operations | 1M operations | $0.40 | <https://example.invalid/pricing-queues> (2026-01-01) | assumed, 0.2M operations | 0.2 × $0.40 | $0.08 |
| Analytics Engine events | 1M events | $0.25 | <https://example.invalid/pricing-analytics> (2026-01-01) | assumed, 1M events | 1 × $0.25 | $0.25 |
| Postgres (app + mirror clusters) | cluster-month (fixed) | $10.00 | <https://example.invalid/pricing-postgres> (2026-01-01) | fixed fee at current tier; storage/egress excluded | flat | $10.00 |
| Egress / other metered items | — | $0.00 | not metered in this synthetic example | not measured | — | $0.00 |
| **Total at stated usage** | | | | | | **$12.03** |

At the stated (invented) usage the fixed Postgres fee dominates; R2 storage
is the line that grows with library size. Egress was not metered, so the
total is a floor, not an all-in bill.

### 4. Explicit non-goals

- No production activation, production migration, or production data.
- No real users, real provider accounts, or real content.
- No demand, pricing, or willingness-to-pay conclusion.
- No load, soak, or multi-region claim beyond the single synthetic journey.
- This synthetic example validates the template format only; it validates
  no build.

### 5. Evidence index

| ID | Claim proven | Build + environment | Link | Observed | Result | Verified by |
|---|---|---|---|---|---|---|
| E1 | Deployed staging build identity equals the promoted SHA `0123456789ab` in the synthetic health response | `0123456789ab` · staging | <https://example.invalid/runs/build-identity/1> | 2026-01-14 | PASS | QA, 2026-01-15 |
| E2 | Unauthenticated staging request receives an Access challenge | `0123456789ab` · staging | <https://example.invalid/runs/access-check/2> | 2026-01-14 | PASS | QA, 2026-01-15 |
| E3 | Access audit for the run window shows no bypass policy was used | `0123456789ab` · staging | <https://example.invalid/logs/access-audit/3> | 2026-01-14 | PASS | QA, 2026-01-15 |
| E4 | Synthetic user creates an account and signs in end to end | `0123456789ab` · staging | <https://example.invalid/runs/e2e-staging/4> | 2026-01-14 | PASS | QA, 2026-01-15 |
| E5 | Synthetic provider credentials are accepted by BYOP configuration | `0123456789ab` · staging | <https://example.invalid/runs/e2e-staging/5> | 2026-01-14 | PASS | QA, 2026-01-15 |
| E6 | Stored BYOP credential record holds ciphertext only, with no plaintext value present | `0123456789ab` · staging | <https://example.invalid/records/byop-storage/6> | 2026-01-14 | PASS | QA, 2026-01-15 |
| E7 | Synthetic provider credential value is absent from the run logs (names only) | `0123456789ab` · staging | <https://example.invalid/logs/redaction-query/7> | 2026-01-14 | PASS | QA, 2026-01-15 |
| E8 | Provider credential value is masked and not displayed in the UI after entry | `0123456789ab` · staging | <https://example.invalid/runs/e2e-staging/8> | 2026-01-14 | PASS | QA, 2026-01-15 |
| E9 | Synthetic fixture ingests to a playable library item | `0123456789ab` · staging | <https://example.invalid/runs/e2e-staging/9> | 2026-01-15 | PASS | QA, 2026-01-15 |
| E10 | Player spins indefinitely after seeking past the buffered range | `0123456789ab` · staging | <https://example.invalid/runs/e2e-staging/10> | 2026-01-15 | FAIL | QA, 2026-01-15 |
| E11 | Metering assertion was skipped, so no metering result was produced | `0123456789ab` · staging | <https://example.invalid/runs/e2e-staging/11> | 2026-01-15 | SKIPPED | QA, 2026-01-15 |
| E12 | Last-good SHA `89abcdef0123` is recorded pre-promotion with a push-triggered `deploy.yml` run whose `staging-web` conclusion is `success` (not `skipped`) | `89abcdef0123` · staging | <https://example.invalid/runs/deploy/12> | 2026-01-13 | PASS | QA, 2026-01-15 |
| E13 | Workers deployed versions and their rollback targets are recorded separately pre-promotion | `89abcdef0123` · staging | <https://example.invalid/records/workers-rollback/13> | 2026-01-13 | PASS | QA, 2026-01-15 |
| E14 | Confirmation screen on one locale displays a raw message key instead of localized text | `0123456789ab` · staging | <https://example.invalid/runs/e2e-staging/14> | 2026-01-14 | FAIL | QA, 2026-01-15 |
| E15 | Previous promoted build has no must-pass `FAIL`, so this is the first consecutive failing promoted build | `89abcdef0123` · staging | <https://example.invalid/records/prior-readout/15> | 2026-01-13 | PASS | QA, 2026-01-15 |
| E16 | Section 3 line items sum to the $12.03 monthly estimate at the stated usage | `0123456789ab` · staging | <https://example.invalid/records/cost-total/16> | 2026-01-15 | PASS | QA, 2026-01-15 |
| E17 | Scan of the run logs and evidence artifacts finds no secret value or personal data | `0123456789ab` · staging | <https://example.invalid/logs/secret-scan/17> | 2026-01-15 | PASS | QA, 2026-01-15 |

### 6. Kill rules and verdict

| ID | Condition (pre-registered) | Source of threshold | Observed value | Evidence | Verdict |
|---|---|---|---|---|---|
| K1 | Any must-pass synthetic-flow step `FAIL` on two consecutive promoted builds | Launch-quality bar for the core journey | 1 consecutive failing build (step 5 on this build; none on the previous promoted build) | E10, E15 | NOT TRIGGERED |
| K2 | Estimated monthly infra cost at stated usage exceeds $50 | Pre-launch cost ceiling for the validation phase | $12.03 | E16 | NOT TRIGGERED |
| K3 | Any secret value or personal data found in staging logs/evidence during the run | "Must never contain" rules in this template | None found | E17 | NOT TRIGGERED |

```text
VERDICT: NO-GO — must-pass step 5 (play/seek) failed per E10; no kill rule triggered (K1–K3) (product lead, 2026-01-15)
```

### 7. Sign-off

- QA: every evidence link in section 5 was opened and proves its claim;
  deployed-build identity was verified against the promoted SHA per E1,
  and the `SKIPPED` metering assertion in E11 is recorded as
  `INCONCLUSIVE`, not a pass. `QA, 2026-01-15`
- Decision owner: verdict above recorded; non-goals in section 4 stand.
  `product lead, 2026-01-15`
- What could not be verified: egress cost (not metered in this synthetic
  example).
