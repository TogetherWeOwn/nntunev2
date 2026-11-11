# Candidate staging admission receipts

This is a necessary source-level production prerequisite, not operational
acceptance or permission to deploy. The manual production lane remains disabled
by default. No runner, migration, mirror, production binding, secret or promoter
is enabled by this change.

## Current state: no approved product scope

`staging-admission-policy.json` deliberately has **no approvals**. A green legacy
journey, its permitted parked skips and a successful but unconfigured API job
cannot qualify a candidate. The producer can publish an `incomplete` diagnostic
receipt. Real production admission refuses until an independently approved
product contract and complete candidate execution evidence exist. Do not enroll
one of the synthetic test contracts as a production approval.

Product requirements/applicability belong to the product authority; QA separately
reviews their executable evidence mapping. The repository's normal protected
review process must authenticate those decisions before a policy entry is added.
This interface does not select or reduce the applicable product requirements.

## Reviewed input encoding

The contract is the exact UTF-8 bytes of `ci/product-journey-contract.json` (not
implicitly reserialized). It has `schema: 1`, `evaluator: "junit-map-v1"`, an
immutable `spec_revision` and a nonempty `criteria` array. Each criterion has a
stable unique `id` and boolean `applicable`. An exclusion requires an approved
`rationale`; an applicable criterion requires nonempty `evidence`, each item
containing exactly `classname`, `name` and `outcome: "passed"`. Selectors identify
individual JUnit cases, must be unique and must occur exactly once as passed.
Missing, retried/ambiguous, skipped, parked, unconfigured or failed evidence does
not satisfy a criterion. The trusted reporter preserves retry failure markers;
`fail` annotations and flaky/rerun failure or error elements cannot count as a
passed product criterion even when Playwright calls the overall test green.
No skip is automatically an applicability exclusion. Unsupported proof types
require reviewed evaluator changes, not boolean inputs.

The trusted policy has `schema: 1`, the evaluator and exactly one current approval
when available. An entry pins `sha256` of the exact contract bytes, the same
`spec_revision`, and immutable repository-safe `product_approval` and `qa_approval`
references. Approval/revocation is a reviewed policy change, never an artifact's
`approved` field or a dispatch input. The production CLI offers no policy path,
API origin, local receipt, acceptance boolean or contract-hash override.

The migration binding is SHA-256 of the existing candidate
`migrations/manifest.toml` bytes. It is an envelope fingerprint of that existing
manifest, not a new migration inventory or generation scheme. The existing
per-SQL byte checksums, numbering/drift gate and read-only deployed schema gate
remain authoritative and unchanged; this fingerprint does not certify a database
or authorize executing any migration.

## Producer and consumer

`deploy.yml` passes the actual `staging-api` outputs into its same-revision reusable
journey workflow. After the existing legacy integrity gate, the receipt records
fixed outcome labels, exact source/root run/attempt, migration and product hashes,
coverage and both live identity observations. The shared live helper adds only
SHA/timestamp annotations after both web/API assertions pass. It never annotates
same-build success for an unconfigured or skipped API. The producer reads JUnit
but uploads only the bounded whitelisted receipt, not JUnit, URLs, accounts,
credentials, logs or raw diagnostics. The existing failure sanitizer is unchanged.

The production verifier is checked out from the main dispatch's reviewed source,
not the selected candidate or an artifact. It requires authenticated GitHub
workflow/repository/run/attempt/job/step and artifact association. It reads one
fixed bounded ZIP member in memory, checks the archive's SHA-256 against Actions'
immutable digest and recomputes receipt satisfaction against the trusted policy.
It separately checks the candidate's manifest from Git objects. Full source
objects must exist on main. Relevant producer/evaluator/policy source must match
the trusted verifier and observed current main; unrelated main movement is not
an expiry rule. Listings that cannot prove completeness refuse rather than
silently selecting from a partial page. A superseding attempt invalidates the old
receipt. A known conflicting intervening staging deploy invalidates old evidence.

Current main is resolved independently of checkout's `origin/main`: the pinned
checkout action can reset that tracking ref to the captured dispatch commit when
main advances before checkout. The verifier queries the remote's exact
`refs/heads/main`, fetches the resolved immutable commit without writing tracking
refs or `FETCH_HEAD`, verifies its commit identity, and repeats the remote query
to refuse movement during resolution. Missing, ambiguous, unavailable or
incoherent resolution refuses; the dispatch commit must remain an ancestor.
All current-main ancestry, producer/policy and manifest comparisons then use
that one pinned SHA, never a mutable tracking ref. The trusted dispatch checkout
and policy remain unchanged. This is a point-in-time source snapshot, not a
reservation of main or staging and not operational acceptance.

Source binding compares Git tree records (object IDs, names, types and modes),
not selected helper bytes or the whole repository commit. The complete committed
`e2e/` tree binds every mapped test, imported/transitive helper, fixture, script,
Playwright/TypeScript configuration and reporter/runtime package and lock input.
No TypeScript import parser or contract-selector-to-filename guess is involved:
all specs load in the staging journey, so the conservative superset is deliberate.
Generated/untracked reports, installed packages and caches are not Git inputs;
`npm ci` and the workflow-pinned toolchain supply the locked runtime.

The suite also reads or executes repository inputs outside `e2e/`. The binding
includes `.github/`, `fixtures/cc0/`, `config/`, `workers/` and
`crates/nntune-config/` trees, plus `bin/smoke.mjs`, `Cargo.toml`, `README.md`,
`apps/web/wrangler.toml`, `crates/nntune-core/src/transcode.rs`,
`containers/mb-import/Dockerfile`, `docs/api-errors.md` and `docs/cost-basis.md`.
The evaluator, policy and contract remain explicitly bound. These conservative
roots include source-scanned fixtures and worker-lockfile discovery, not just
executable imports. Missing roots, non-tree roots and non-regular explicit files
refuse. Adding dependencies outside these roots requires a reviewed binding
update along with the test/runner change; do not narrow the roots to whatever
happens to pass an older receipt. Changes elsewhere (for example an unrelated
operations document) remain allowed and do not create a commit-age expiry rule.

Candidate discovery queries the exact source SHA, not all lifetime history.
Conflict discovery includes every run created within 66 days before the opening
identity observation, plus independent all-time queries for queued, in-progress,
waiting, pending and requested runs. The window is a conservative execution
horizon, not a receipt TTL: GitHub permits reruns only within [30 days of the
initial run](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/re-run-workflows-and-jobs)
and limits each run, including waiting and approval, to [35 days](https://docs.github.com/en/actions/reference/limits).
The extra day and UTC date rounding include the boundary. Thus an old run that
was rerun or waited before deploying late is included; a small creation-only
lookback would not be sufficient. Revisit this horizon if those platform limits
change. Each query must return a complete, stable-count, duplicate-free listing
below GitHub's 1,000-result search ceiling (up to ten pages, 4 MiB per page).
Missing pages, inconsistent counts, ceiling exhaustion, malformed evidence and
the existing 120-second aggregate network deadline refuse admission.

Completed runs last updated before the opening observation cannot contain a
later completed job in that snapshot. For other conflicting runs, inspect jobs
from **all attempts**, not only the latest rerun. An unresolved rerun must also
expose both deployment lanes in its current attempt; historical terminal lanes
cannot substitute for current-attempt evidence. Missing deployment lanes,
nonterminal jobs (even with a stray skipped conclusion), absent conclusions,
actual deployment execution after opening and uncertain mutation all refuse.
A completed skipped job is non-mutating. A later failed job is also non-mutating
only when the reviewed producer source matches, an earlier prerequisite failed,
and the uniquely identified deployment step explicitly completed as skipped.
A failed or cancelled deployment step is not proof that no mutation occurred.
Whole-job completion time alone must not reject a proven pre-deployment failure.
The existing single-page job/artifact bounds remain; incomplete attempt/job
history refuses rather than hiding a prior mutation behind a later failed rerun.

The admission point also rereads the existing public web/API identity endpoints
with the existing staging Access pair, refusing redirects and identity drift.
No Access credential follows a redirect; GitHub archive redirects drop the
repository token before crossing origins. API/network/download failures refuse.

Freshness means exact source/attempt/manifest/approval coherence and drift checks,
not a new age TTL, soak, passive-production baseline or continuous staging
reservation. Per-lane locks do not prove uninterrupted staging ownership. These
observations do not replace independent runtime QA, incident/drill clearance,
release acceptance, ledger/N-1 compatibility or separate operator execution.

## Offline verification

`python3 -I ci/test_staging_admission.py` uses synthetic policy and authenticated
API-shaped fixtures without a network, deploy or operational acceptance claim.
It is included in the existing required `release-proof` CI job. The production
workflow's old inline job checks are only preliminary guards, separately covered
by `e2e/tests/deploy-production.spec.ts`; they cannot bypass the receipt verifier.

AGENTS.md “Clean-room, YouTube and MetaBrainz guardrails”: this change handles
admission metadata only; no UI, playback, catalogue ingestion or cover-art rights
behavior changes. No per-lane lock is represented as a shared reservation.

REA angle: reuse the shipped API outputs, JUnit integrity gate, live build checks,
migration manifest and successful-release Actions provenance pattern, rather than
introducing an evidence service or replacement acceptance programme.
