# Paid invite staging acceptance

Use one protected staging candidate for Web, API and Stream. Record the candidate
commit, deployed identities and hosted job URL. Use synthetic audio, a synthetic
invited account and Stripe test mode. Qualify live invoice history and effective
database privileges before paid activation. Follow the [staging promotion checklist](ops.md#staging-promotion-checklist)
and [M1 traceability matrix](m1-exit-traceability.md).

The API CI case `paid_invite_signup_to_subscription_cap_and_lapse` rehearses
the signup, signed Stripe deliveries, recovery of a selected renewal and
play refusals with the in-memory store and Stripe transport. The recording
below verifies hourly dispatch and the browser, provider and database on the
deployed build.

The API CI case `paid_invite_resumes_interrupted_checkout_before_verified_subscription`
rehearses a lost Checkout session mark after signup. Signing in again with the
same passkey recovers the original customer and session. Signed events for an
unpaid completion and a completion with a mismatched subscription both leave
playback gated. The fixture stops before invoice reads.

## Run the protected evidence check

The [runner](../.github/scripts/paid_invite_acceptance.py) is off unless
`NNTUNE_PAID_INVITE_ACCEPTANCE=1`. Use the commands below in the protected
staging Actions job at the candidate checkout, once that job and its private
evidence handoff are provisioned. The current API job uses disposable
loopback only. The runner makes HTTPS reads to the fixed staging host and
Stripe. Run both phases during one protected staging window.
Set `EXPECTED_BUILD_SHA` from the protected staging deploy's full commit
SHA. Supply the staging Access service token as `CF_ACCESS_CLIENT_ID` and
`CF_ACCESS_CLIENT_SECRET`. Use an approved test-mode
`PAID_INVITE_STRIPE_READ_KEY` with read access to Balance, Account, Checkout
Sessions and Subscriptions. Use a restricted key where available. Feed the
private evidence JSON on stdin.

    python3 -I .github/scripts/paid_invite_acceptance.py admit < admission.json
    python3 -I .github/scripts/paid_invite_acceptance.py finish < completed.json

Both records contain `app` (`nntune-v2`), `environment` (`staging`),
`origin` (`https://staging.nntune.com`), `build` (the candidate SHA),
`stripe_account` (the independently verified Stripe test account ID) and
`checks` (an object mapping each observed check below to its verified full
build SHA). The operator enters a check after verifying the underlying
evidence. Keep the source observations and input JSON in the protected
runner. Retain only the output verdict and protected job URL in the readout.

| Admission check | Required observation |
| --- | --- |
| `staging_isolated` | Staging app, database and storage isolated from production and member data. Paid dispatch initially off. |
| `invoice_history` | The separate #1809 comparison reports complete and collision-free on the candidate. |
| `effective_privileges` | The actual staging billing roles have the required effective grants. |
| `stream_deployed` | Same-run staging Stream deploy executed, with the candidate SHA and its version verified on the bound service. |
| `stripe_configuration` | Test account, portal, prices, webhook, hourly job and monthly renewal timing verified as described below. |
| `synthetic_scope` | Fresh single-use invite and synthetic account, audio and test payment selected for this run. |
| `teardown_ready` | Staging-only authority and an inventory of the specific resources this run may retire. |

After an `admitted` result, use the default-off #1787 route only for the
protected test window and execute the recording below. `finish` takes the
same admission fields and checks plus the following checks and a
`checkout_session` field, the original test
Checkout Session ID. Verify the deployed identities on any staging
configuration change and abort the run on deploy drift.

| Finish check | Required observation |
| --- | --- |
| `admitted` | The opening check succeeded on this SHA for the same run and test account. |
| `invited_signup` | The synthetic signup succeeded and a second redemption refused. |
| `payment_applied` | The selected Checkout Session belongs to this signup. Stripe's signed delivery applied coverage once. |
| `playback_and_meter` | Synthetic audio played and continued after seek, with delivered usage and stable totals on replay. |
| `playback_cap` | New play refused at cap. Continuation followed its lease deadline. |
| `renewal` | Coverage advanced from the selected renewal, with invoice-history and scheduled dispatch observed as below. |
| `cancellation_or_lapse` | Period-end cancellation or lapse was applied and observed on the same subscription. |
| `teardown_verified` | Paid dispatch is off again and synthetic resources are retired as described below. |

Before entering `playback_and_meter`, verify that the play, seek, delivered
usage and replay observations came from the synthetic account and playback
sequence in this admitted run. An earlier playback sequence for the same
account and build cannot supply that attestation. The runner verifies the
attested build SHA and reads back the selected Checkout Session by ID. The
operator verifies playback provenance because the receipt carries only a
build SHA for `playback_and_meter`.

On success or interruption, close paid dispatch in the protected staging
configuration first. Disable the dedicated test webhook destination, confirm
Checkout refuses, and restore the pre-trial Access policy and settings.
Retire only the run's tracked test checkouts,
subscription, invite, account/session and synthetic media using their
staging lifecycle controls. Verify no open checkout, active test subscription
or usable invite/session remains. Keep the finished Stripe payment records
available for readback. The closing check reads the paid test session and
its canceled subscription directly and rechecks Web and API build identity.
An incomplete teardown leaves the run open.

Hosted CI runs this contract on disposable fixtures and runs #1803's signed
subscription and playback-cap journey. A protected staging run still needs
candidate-specific #1809 compatibility, effective billing privileges, approved
test-mode credentials, synthetic playback and the protected Actions job.
The selectable Stripe prices are monthly. Hosted fixtures rehearse a fresh
customer created with the selected test clock before Checkout, with paid
dispatch off. The rehearsal refuses an existing binding, an unrelated
customer and a live or mismatched clock response. The API fixture follows
the selected Checkout Session and subscription from signed Checkout through
synthetic renewal. The loopback runner fixture rehearses closing readback
before clock retirement.

The offline cancellation fixture checks the Checkout return destination, then
verifies that the invited listener stays unsubscribed and playback stays gated.
After the abandoned session expires in the Stripe fixture, the same listener
can explicitly open a fresh Checkout Session.

The browser Checkout path does not supply a test clock. Before arming the
protected trial, still verify in the approved Stripe test account that a
fresh customer can be created under the selected clock, that Checkout uses
that exact customer and that advancing the clock renews the same monthly
subscription inside the staging window. Record the clock, customer,
Checkout Session and subscription IDs privately. Close paid dispatch first,
cancel the selected subscription and complete the runner's closing
readback while its Stripe records are still available. Retire the clock
after that readback. The fixture does not exercise Stripe clock timing or
deletion against the provider.

## Record the staging journey

1. Open an owner-issued single-use staging invite to prefill the code.
   Follow [signup arrival](../e2e/tests/signup-arrival.spec.ts) for email-code
   and passkey handling. Confirm a second redemption refuses.
2. Keep `PRIVATE_LISTENING_COHORT` absent for the paid run. Confirm
   `INVITES_REQUIRED`, `PAID_INVITE_BILLING=1`, Stripe test credentials and a
   test portal. Verify that the configured portal exists in the same Stripe
   test account with period-end cancellation and subscription changes disabled.
   Verify `plan_prices` against the provider account, currency, amount, interval
   and usage type. Admit only `POST /v1/billing/webhook` via the Access exception.
   Use a `2024-12-18.acacia` test destination for the three events below.
   Check invalid signatures and checkout refusal without a portal.
3. Deliver `checkout.session.completed` after checkout. Verify a replay
   preserves the subscription and invoice history. Confirm coverage and portal
   through `/v1/me/subscription` and the billing page. Reuse the
   [play, seek and meter assertions](../e2e/tests/ingest-play-meter.spec.ts)
   with the rank 74 encrypted fixture and callable browser relay. Assert 206
   and `Content-Range`, advancing audio, a UI seek, continued playback and
   positive delivered usage from `/v1/me/usage`. Replay one meter window and
   verify the usage total stays flat.
4. Cross the cap and verify a new play returns `plan_limit_exceeded`. Check
   continuation before and refusal after its lease deadline. Below cap, observe
   coverage and invoice history advance before the hourly job after delivering
   only `invoice.paid`. On the next renewal, withhold both webhook signals and
   verify the hourly job advances both records.
   Schedule cancellation and replay `customer.subscription.updated`. Confirm
   the flag and stable history before the `lapsed` refusal at cutoff. Run the
   [same-build privacy cases](m1-exit-traceability.md).
5. Repeat the private listening refusal control in the personal configuration
   on that build. Keep credentials, invite links, and hosted session URLs out
   of artifacts. Record each verdict. A missing or skipped step leaves
   acceptance open.

Before testing checkout, confirm hourly trigger `0 * * * *` fires on the same
build. Before withholding renewal webhooks, confirm the synthetic subscription
is due by PostgreSQL time and appears in the next hourly batch of up to 16
targets. The [sweep](../workers/api/src/billing_sweep.rs) requires an eligible
status and absent or elapsed `paid_through_at`. The Stripe test clock advances
provider time. Resolve ambiguous old payments before reusing their flight. Remove
`PAID_INVITE_BILLING` to close paid dispatch.
Check Worker error logs for unresolved scheduled targets.

## Invoice history preflight

Run the [read-only preflight](../.github/scripts/billing_history_preflight.py) with
Python from the exact candidate checkout in the approved private runner.
Set `NNTUNE_INVOICE_PREFLIGHT=1`, `BILLING_PREFLIGHT_HOST` to the independently
verified staging origin on port 5432, and `BILLING_PREFLIGHT_ROLE` to its
approved observer login. Supply `BILLING_PREFLIGHT_PASSWORD` as a runner secret.
The observer needs global invoice-key visibility and access limited to the
required columns and prepared evidence. The client verifies TLS, database and role.

Pin the schema, Stripe test account and cutoff. Prepare one stable batch in
`billing_preflight.scope` and `billing_preflight.projection`. Both use text
`stripe_invoice_id` (globally unique), `user_id` and `stripe_subscription_id`.
Scope covers existing subscription targets, including retained targets that can
return to advancing states. Inventory the union of relevant retained and eligible
keys at the cutoff, set boolean `retained` per key and pass the independent
`BILLING_PREFLIGHT_SCOPE_COUNT` (1–10,000). The paid invite journey covers first-time Checkout.
Align the archive snapshot with the evidence cutoff. If the archive changes
after that cutoff, prepare a new inventory and projection at a common observation.

Project the candidate's fold using current target and price bindings at the same
observation. Set boolean `evidence_complete` after verifying ownership and
provider bindings and exhausting invoice and line pages. Each projection carries
text `plan_id` and `currency`, bigint `amount_cents`, `start_s`, `end_s` and `paid_s`
(Unix seconds), and paired bigint arrays `starts` and `ends` for up to 1,000 half-open
periods. Two empty arrays mean unknown coverage. Reporting endpoints may coincide.

The report is one JSON object with `total`, `exact`, `eligible_hydration`, `fresh`,
`collision` and `incomplete` counts. Exit 0 means complete and collision-free.
Exit 1 means collision. Exit 2 means incomplete evidence, including missing targets
or projections. An inventory mismatch exits 2 with `scope_incomplete` instead.
Exit 3 gives `disabled` or `refused`. Keep identities, projections, credentials and DB
diagnostics private. Recheck on drift. Writer privileges and legacy provenance are separate prerequisites.

### Subscription lock privilege

The owned billing transaction takes a `SHARE ROW EXCLUSIVE` lock on
`public.subscriptions` for stale no-ops as well as writes. For the writer
grant checked by #1768, column-level `UPDATE` is insufficient.

The [catalog query](../.github/scripts/billing_subscription_lock_privilege.sql)
checks effective table-level `UPDATE`, including inherited grants. From a
read-only transaction on the independently pinned database, load the query
with `psql` using `ON_ERROR_STOP=1` and the explicit `database` and
`billing_role` variables. Use the billing writer login name from the
connection configuration and set the transaction's search path to
`pg_catalog, pg_temp`. Require one true result for this grant and verify the
remaining billing privileges separately before recording `effective_privileges`.
