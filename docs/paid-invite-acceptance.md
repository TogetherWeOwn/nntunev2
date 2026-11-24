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
