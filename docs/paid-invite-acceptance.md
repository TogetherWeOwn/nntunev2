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
build. It rotates through up to 16 due targets. Keep test timing within one
batch. Resolve ambiguous old payments before reusing their flight. Remove
`PAID_INVITE_BILLING` to close paid dispatch.
Check Worker error logs for unresolved scheduled targets.

## Invoice history preflight

Run the [read-only preflight](../.github/scripts/billing_history_preflight.py)
from the exact candidate checkout inside an approved private runner that
holds the prepared evidence and the staging observer credential. It runs
only with `NNTUNE_INVOICE_PREFLIGHT=1`. Set `BILLING_PREFLIGHT_HOST` to the
independently verified staging PostgreSQL origin on port 5432 and
`BILLING_PREFLIGHT_ROLE` to its approved restricted login. Supply
`BILLING_PREFLIGHT_PASSWORD` through the runner's secret environment.
The client verifies TLS and checks the session's database and role before
reading evidence. The observer needs global invoice-key visibility and
reads limited to the required columns and prepared evidence.

Pin the candidate, staging origin, schema, Stripe test account, scope and
evidence cutoff. In the approved perimeter, prepare a fixed batch in
`billing_preflight.scope` and `billing_preflight.projection`. Both use text
`stripe_invoice_id` (unique globally), `user_id` and `stripe_subscription_id`.
Scope is the independently inventoried union of relevant retained and currently
eligible keys, including retained targets that can return to an advancing state.
Set the boolean `retained` per scope key and pass the independent denominator
as `BILLING_PREFLIGHT_SCOPE_COUNT` (1–10,000). Use stable relations for one
batch and keep the scope and archive aligned to the same observation.

Use the candidate's fold at the pinned time for each projection. Set the
boolean `evidence_complete` only after checking exact ownership and provider
bindings and exhausting both invoice and line pages. The projection carries
text `plan_id` and `currency`, bigint `amount_cents`, `start_s`, `end_s`
and `paid_s` (whole Unix seconds), and paired bigint arrays `starts` and
`ends` for up to 1,000 half-open periods. Two empty arrays represent
unknown exact coverage. A missing projection counts as incomplete.

The command emits one JSON object of counts (`total`, `exact`,
`eligible_hydration`, `fresh`, `collision`, `incomplete`) for an admitted
comparison. Exit 0 requires a complete batch without collisions. Exit 1
reports a collision, including when other items are incomplete. Exit 2
reports incomplete evidence or the fixed diagnostic `scope_incomplete`
when the inventory does not reconcile. Exit 3 gives a fixed `disabled` or
`refused` diagnostic. Keep identities, projections, credentials and database
diagnostics inside the approved perimeter. Recheck if scope, inputs or candidate
drift. Effective writer privileges and any legacy provenance decision are
separate activation prerequisites.
