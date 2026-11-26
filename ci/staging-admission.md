# Candidate staging admission receipts

This is a necessary source-level production prerequisite, not operational
acceptance or permission to deploy. The manual production lane remains disabled
by default. No runner, migration, mirror, production binding, secret or promoter
is enabled by this change.

## Product approval and candidate evidence

An empty `staging-admission-policy.json` approval list refuses admission. A green
legacy journey, its permitted parked skips and a successful but unconfigured API
job cannot qualify a candidate. The producer can publish an `incomplete`
diagnostic receipt. Production admission requires both an independently approved
product contract and complete candidate execution evidence. Do not enroll a
synthetic test contract as a production approval.

Product requirements/applicability belong to the product authority; QA separately
reviews their executable evidence mapping. The repository's normal protected
review process must authenticate those decisions before a policy entry is added.
This interface does not select or reduce the applicable product requirements.

The offline `release-proof` CI job checks the installed policy with the same
approval check as the verifier. When the contract file is absent, CI requires
an empty approval list and expects the check to refuse. When the file is present,
the check must accept its exact bytes using the installed policy.

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
change. Creation-date discovery uses two inclusive 22-day UTC ranges starting
at the rounded cutoff, then an open-ended range starting on the next UTC date.
The ranges do not overlap and the open tail includes pushes after the journey
opened. The five all-time unresolved searches still have no date filter.

Each query must return a complete, stable-count, duplicate-free listing below
GitHub's 1,000-result search ceiling (up to ten pages, 4 MiB per page). A recent
unresolved run can appear in both searches, but both records must agree. Out-of-scope
rows, missing pages, inconsistent counts, ceiling exhaustion, malformed evidence
or expiry of the shared 120-second network deadline refuse admission. The
verifier never evaluates conflicts from a partial union.

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

## Private music producer interface

The reviewed contract can select `journey: "private-music"`. Contracts without
that field select the ordinary journey. The private contract must map each of
`upload-to-library`, `play`, `seek`, `queue`, `anonymous-refused`,
`unlisted-refused`, `revoked-or-expired-refused` and `video-off` to its own case
with `classname="private-music"` and `name` equal to the criterion ID. Paid
acceptance is separate. Upload evidence requires actual upload and publication
through Library arrival. Playing a prepublished fixture cannot satisfy it.

The protected main-push job `post-deploy private music journey` must complete
`Qualify private listening`, `Record candidate admission receipt` and
`Upload candidate admission receipt` in order, after the web, API and Stream
deployment steps succeed in the same attempt. Its producer calls
`python3 -I ci/staging_admission.py produce-private` with the existing API job
outputs, the bounded report at `e2e/private-staging-results/admission.xml`, and
`PRIVATE_STAGING_DEPLOYMENTS` containing the three deployment pins. Keys are
`staging-web`, `staging-api` and `staging-stream`. Each pin has `worker`,
`build_sha`, `version_id` and `deployment_id` for the deployed staging Worker,
plus decimal string `run_id` and `run_attempt` captured by that deployment job.
Version and deployment IDs are lowercase UUIDs. The producer verifies the
armed Stream mode before recording its pin.

The reporter derives cases from actual protected Playwright results and emits
the observed `retry="0"` on first-attempt cases, including the identity cases.
Missing, duplicate, skipped, expected-failed, retried or failed cases refuse.
The evaluator also requires these JUnit properties on the first-attempt cases:

| Case | Required track properties |
| --- | --- |
| `upload-to-library` | `private-publication`, `private-library` |
| `play` | `private-play` |
| `anonymous-refused` | `private-anonymous-refusal` |
| `unlisted-refused` | `private-unlisted-refusal` |
| `revoked-or-expired-refused` | `private-cutoff-refusal` |

Each property records the observed publication, Library row, player result or
refusal target as a JSON object with `track_id`, `sha256`, `build_sha`, `run_id`
and `run_attempt`. The last two values are decimal strings. Each case supplies
its own observation. All properties must appear exactly once on their mapped
cases and agree with the publication's canonical track UUID. The build, run
and attempt must match the selected Actions evidence, and `sha256` must match
the `tone-a.flac` bytes in that build. The receipt carries the shared identity
as `private_track`. The verifier checks it against the selected attempt and
candidate Git object.

`private-cutoff-refusal` also carries `cutoff_listener` and `refused_listener`.
These run-local UUID aliases must match. The journey assigns them from the
session established by the synthetic invite and the authenticated session
used for the cutoff probes. The aliases stay in JUnit. Session bindings,
email addresses, and credentials stay out of JUnit and the receipt.

The `build-identity` and `build-identity-final` cases use the same class and
carry the existing `candidate-identity` properties, plus a `deployments` map
with the four provider identity fields per lane. Read these independently
from provider state at both boundaries. The verifier compares them with the
deploy job pins. It also binds `fixtures/synthetic-flac/private-staging/` and
checks for intervening mutations in all three lanes, including other root runs
of the same commit. The artifact name adds `-private-music` to the existing
name. Its single `staging-admission.json` member carries the private `journey`
and pins. Raw XML, failure details and credentials stay off the artifact.

Protected producer wiring, live evidence and reviewed approval are separate
release work. Production preflight currently refuses application promotion.
The maintenance bootstrap keeps its ordinary preliminary job check.

### Verified private staging pins

After staging receipt verification and the current Web/API identity readback
succeed, `verify` emits one JSON line under `verified_private_music_staging`.
The object has `schema: 1`, the candidate `sha`, decimal string `run_id` and
`run_attempt`, `artifact_name`, `migration_manifest_sha256`, `contract_sha256`,
`policy_sha256`, and the three validated `deployments` pins described above.
`observed_at` contains the normalized UTC opening `build-identity` and closing
`build-identity-final` observation times. Refusals and ordinary admissions emit
no private pins. The line is staging evidence only. Match it against the
[pinned candidate template](../docs/first-private-music-launch.md#pinned-candidate-template).

### Synthetic handoff when Actions evidence is unavailable

For candidate `<sha>`, the private journey and receipt upload in run
`<run-id>`, attempt `1` succeeded. Actions lists the unexpired receipt. The ZIP
download times out before the verifier can check its digest. If the existing
Actions reader instead cannot retrieve the required job records for that
attempt, record the job read as the blocker and use the same disposition.

| Release record field | Example entry |
| --- | --- |
| Staging attempt | `<sha>`, run `<run-id>`, attempt `1`. Retain the run reference and the job references already available. |
| Staging admission | Pending. The ZIP for `staging-admission-<sha>-<run-id>-1-private-music` could not be fetched. |
| Dispatch readiness | Blocked pending verified staging evidence. |

When the existing authorized Actions read path is available, repeat step 3 of
[Qualify the candidate](../docs/first-private-music-launch.md#qualify-the-candidate)
for the pinned SHA. Recheck the selected run and attempt and verify the exact
archive from that attempt, including its digest and the current staging identity.
Keep admission Pending until the verifier emits matching pins. If expiry, a
superseding attempt or staging drift invalidates the original evidence, return
to candidate qualification for fresh evidence. Keep scheduled trial cleanup
on time while verification is blocked.

### Draft contract for the first private music release

The [draft contract](private-music-journey-contract.draft.json) maps the
[roadmap's first personal-use release](../ROADMAP.md#product-boundaries) to
assertions in the [protected staging journey](../e2e/private-staging/journey.spec.ts),
using the private admission format from
[#1797](https://github.com/TogetherWeOwn/nntunev2/pull/1797). Paid signup is
outside this release scope. The music-only scope requires proof the video
interface is off. The [private listening runbook](../docs/ops.md#private-listening-qualification)
also calls for separate checks of ledger-based cohort revocation, refused and
pre-revoked invites, and the absence of cohort payment history and Stripe
credentials.

Each applicable criterion requires the `private-music` class and the case name
below. These are the assertions the bounded reporter must use for each case.

| Case name | Required staging assertions |
| --- | --- |
| `invite-acceptance` | The private invite completes email and passkey signup, clears the invite fragment and opens Library with an authorized response. |
| `upload-to-library` | The owner uploads the checked-in FLAC, the publication receipt matches its SHA-256 and yields a fresh canonical track ID, and that exact ID becomes visible in the invitee's Library. |
| `play` | The first curated track is the fresh publication. Its audio advances through the authenticated relay, the play acknowledgement agrees with the relay identity, and the range response matches bytes of the uploaded FLAC. |
| `seek` | The paused player completes a mid-track seek and resumes advancing from the sought position. |
| `queue` | Queue order matches the curated Library selection. Next selects the companion track and playback advances for that track. |
| `anonymous-refused` | Signup without an invite and anonymous Library access return 401. |
| `unlisted-refused` | An authenticated session outside the cohort gets 403 from Library and play. |
| `revoked-or-expired-refused` | At the configured cutoff, the same invitee session gets 403 from Library and play, its media range is refused, and the Library shows the denial with neither curated row visible. |
| `video-off` | Now Playing exposes no Music Video tab, iframe or video element. |

The early refusal step collects the anonymous and unlisted statuses.
`assertPrivateRefusals` checks them at cutoff. The reporter can mark those
cases passed only after their status assertions pass at cutoff.

The protected suite uses the private admission reporter. It emits the bounded
per-case report above, including both required build identity cases. Playwright's
built-in JUnit reporter records the enclosing test as one testcase and has no
per-step case or explicit first-attempt marker.

The production verifier reads `ci/product-journey-contract.json`. The
separately named draft has no approval entry. Enrollment requires live
evidence from the exact protected attempt and reviewed Product and QA approval.

### Protected contract enrollment review

On the protected main-push `deploy` run for the candidate, open the Actions
summary for the exact attempt and find **Private music contract enrollment
review evidence**. Match the workflow run ID, attempt and build SHA to the
candidate being reviewed. QA compares the
class, case names and first-attempt outcomes with the applicable selectors in
the draft. Both build identity cases are required by the receipt evaluator.
Product reviews applicability, including the paid-signup exclusion.

The summary reads the report directly after successful qualification, before
the receipt step can add provider pins. It is available with an empty approval
policy, even if the receipt step fails. The reporter emits a report only when
all cases pass on their first attempt. An absent summary supplies no case-level
evidence. Treat the Actions summary as public. It exposes only the case table
and public run, attempt and build references. Raw XML, proof values and
diagnostics stay on the runner.
If reviewers require those details, keep approval pending until restricted
custody is available. The summary is review evidence only. The receipt upload
still requires a satisfied receipt, and promotion still uses the independent
verifier.

## Offline verification

`python3 -I ci/test_staging_admission.py DraftPrivateContractTests` runs the
draft selector check against synthetic private-admission and raw Playwright
JUnit shapes without Playwright, staging access or network calls.

The hosted private-music fixture runs the actual Playwright reporter and
summary step offline with disposable data and an empty policy. It compares the
projected run, attempt and build to the synthetic run, and checks that the
summary contains one first-attempt pass per applicable draft selector and per
build identity case. It also checks that paid signup stays excluded and that
a report containing an unexpected paid-signup case emits no summary. This CI
comparison prepares QA for the live review, which still requires evidence
from the exact protected run.

### Accepted automatic push awaiting work

Run `python3 -I ci/staging_admission.py diagnose-pending-push <pinned-sha> <
evidence.json` with saved Actions responses for a first-attempt main push.
Provide the run object as `run` and its complete attempt-specific jobs page
(`total_count` and `jobs`, up to 100 rows) as `jobs`. Capture the two
responses for the same attempt.

An unresolved run with zero job rows points to the attempt's jobs endpoint.
A queued full-journey job with runner ID `0`, an empty runner name and no steps
points to that job's endpoint. Actions can populate `started_at` on a queued job
before runner assignment. The diagnostic prints the observed IDs and the next
read on stderr, exits 1 and emits no receipt or admission pins. Use the normal
receipt verifier for a completed success. A rejected push or a failed journey
does not receive a pending-work diagnosis.

### Receipt verification fixtures

`python3 -I ci/test_staging_admission.py` uses synthetic policy and authenticated
API-shaped fixtures without a network, deploy or operational acceptance claim.
It is included in the existing required `release-proof` CI job. The production
workflow's old inline job checks are only preliminary guards, separately covered
by `e2e/tests/deploy-production.spec.ts`; they cannot bypass the receipt verifier.

### Automatic push rejected before jobs

For an offline rehearsal, run `python3 -I ci/staging_admission.py
diagnose-terminal-push <pinned-sha> < evidence.json`. The JSON object has `run`,
`jobs` (the attempt's `total_count` and `jobs`), and `annotations` (an array of
workflow annotations). Use the synthetic fixture to pair all three inputs for
the same attempt. The diagnostic relies on that pairing because the annotation
rows carry no run or attempt ID.

A failed first-attempt main push with zero jobs and the `deploy-staging-main`
queue-full annotation leaves exact-build qualification pending. The next eligible
automatic staging admission can provide evidence for the build it runs. A later
push with a different SHA needs its own qualification. When the same pinned
attempt instead has a failed full E2E journey step, inspect that job and return
to candidate qualification. Both diagnostics exit unsuccessfully and emit no
admission pins. The synthetic successful control for the same source still
verifies through the receipt reader.

AGENTS.md “Clean-room, YouTube and MetaBrainz guardrails”: this change handles
admission metadata only; no UI, playback, catalogue ingestion or cover-art rights
behavior changes. No per-lane lock is represented as a shared reservation.

REA angle: reuse the shipped API outputs, JUnit integrity gate, live build checks,
migration manifest and successful-release Actions provenance pattern, rather than
introducing an evidence service or replacement acceptance programme.
