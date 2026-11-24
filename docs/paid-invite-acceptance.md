# Paid invite staging acceptance

Run this journey on the same staging build across the Web, API and stream
Workers. Record the candidate commit, deployed build identities and the
hosted job URL. Use a synthetic invited account, synthetic audio and Stripe
test mode. A parked or skipped step leaves paid activation gated.

The [M1 exit traceability matrix](m1-exit-traceability.md) maps the signup,
playback and evidence contracts to existing tests. The
[staging promotion checklist](ops.md#staging-promotion-checklist) defines
build identity and staging admission. Before enabling paid traffic, qualify
live invoice history and effective database privileges for the owned billing
transaction, then attach the complete same-build journey evidence.

## Staging run

1. Confirm the protected staging deployment reports the candidate identity
   from the Web and API. Use the stream deployment from that same candidate.
   Use a dedicated invited account and a published synthetic fixture with
   the rank 74 encrypted source and a callable browser relay. Reuse
   [signup arrival](../e2e/tests/signup-arrival.spec.ts) for email-code and
   passkey handling. Open the single-use staging invite link to prefill
   the code before submitting. Repeat redemption and verify refusal.
2. Keep `PRIVATE_LISTENING_COHORT` absent in the isolated paid candidate.
   The paid path requires `INVITES_REQUIRED`, explicit
   `PAID_INVITE_BILLING=1`, Stripe test credentials and the test portal
   configuration ID. Qualify the exact `plan_prices` test mapping against
   the provider account, currency, amount, interval and usage type before
   enabling the path. Check that the test portal offers payment management
   and period-end cancellation, with subscription changes disabled.
   The Access exception admits only
   `POST /v1/billing/webhook`. Prove signature failures through the
   same ingress, then repeat the verified test delivery and compare the
   resulting subscription and paid-invoice history.
3. Follow the actual checkout redirect with a test payment, then read
   `/v1/me/subscription` for paid coverage and open the same account's
   portal. With the same session and Library fixture, reuse the
   [play, seek and meter assertions](../e2e/tests/ingest-play-meter.spec.ts)
   against the issued media route. Observe 206 and `Content-Range`, audible
   playback position advancing, a UI seek, and positive delivered usage from
   `/v1/me/usage`. Verify that a range replay or retry does not double-count
   the same accounting window.
4. Use bounded synthetic usage to cross the plan cap and assert a new
   `POST /v1/plays` returns `plan_limit_exceeded`. Compare with a valid
   continuation before the lease deadline and a refusal after it. In a
   below-cap period, observe a successful renewal through hourly reconcile,
   then schedule period-end cancellation, confirm the cancellation flag
   reaches the API, and verify `reason: lapsed` at the paid cutoff. Run the
   [privacy cases](m1-exit-traceability.md) against this same build when
   the journey creates retained account data.
5. Keep credentials, invite links and customer or session URLs out of
   artifacts. Record pass or fail per step, with aggregate counts and
   candidate/run identity. An unmounted relay, missing fixture or skipped
   assertion leaves this acceptance open.

Confirm the deployed hourly trigger `0 * * * *` runs on the same candidate
before taking test payments. It rotates through up to 16 due targets per
run and re-reads recorded checkout sessions. Keep the isolated cohort
within one batch when timing the renewal and cancellation checks. Resolve
ambiguous sessionless payments and remotely collectible subscriptions
before reusing the flight. This slice leaves subscription upgrades out of
the purchase flow. Remove `PAID_INVITE_BILLING` to close paid dispatch.
Private listening remains the exclusive playback policy when configured.
