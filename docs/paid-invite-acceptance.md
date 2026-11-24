# Paid invite staging acceptance

Run this journey on the same staging build across the Web, API and stream
Workers. Record the candidate commit, deployed build identities and the
hosted job URL. Use a synthetic invited account, synthetic audio and Stripe
test mode. A parked or skipped step leaves paid activation gated.

| Step | Required observation |
| --- | --- |
| Invite | An owner-issued invite reaches the real signup flow, is consumed once, and the resulting session reaches billing. A visitor without a session cannot create checkout or portal sessions. |
| Checkout | The account selects the exact server-mapped test price. Repeat delivery and retries converge on one checkout and subscription. The return URL alone does not grant entitlement. |
| Entitlement | A verified Stripe event reconciles the exact customer, subscription, item and price with paid invoice evidence. Forged events and a different target leave entitlement unchanged. Portal opens for the same account. |
| Playback and seek | Use the issued play and stream path for a staged synthetic track. Browser audio advances, a seek changes position, and playback resumes from the new position. Observe the real range response and account usage increasing. |
| Cap and lapse | Cross the supported cap with the same account. A new play refuses at the cap; a valid continuation follows the lease contract. After paid coverage lapses, a new play refuses even below cap. |
| Personal isolation | An eligible personal account follows the personal entitlement path while paid dispatch is disabled. Billing does not participate in that path. |

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
   passkey handling. Start it with a real single-use staging invite.
2. Keep `PRIVATE_LISTENING_COHORT` absent in the isolated paid candidate.
   The paid path requires `INVITES_REQUIRED`, explicit
   `PAID_INVITE_BILLING=1`, Stripe test credentials and the test portal
   configuration ID. Qualify the exact `plan_prices` test mapping against
   the provider account, currency, amount, interval and usage type before
   enabling the path. The portal configuration exposes payment management
   and cancellation. The Access exception admits only
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
   below-cap period, advance the test subscription through lapse and
   assert `reason: lapsed`. Run the
   [privacy cases](m1-exit-traceability.md) against this same build when
   the journey creates retained account data.
5. Keep credentials, invite links and customer or session URLs out of
   artifacts. Record pass or fail per step, with aggregate counts and
   candidate/run identity. An unmounted relay, missing fixture or skipped
   assertion leaves this acceptance open.

The optional hourly trigger is `0 * * * *`. It uses the exact-target fold
for due subscriptions and re-reads recorded checkout sessions. It reports
failure above 16 due targets, and leaves ambiguous sessionless payments and
remotely collectible subscriptions held for resolution. This pilot path
requires that bounded due-set and excludes subscription upgrades. Remove
`PAID_INVITE_BILLING` to close paid dispatch. Private listening remains the
exclusive playback policy when its cohort setting is present.
