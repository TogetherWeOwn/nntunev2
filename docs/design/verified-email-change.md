# Verified account email change — static design

**Status: proposal; implementation NO-GO pending the dispositions in §10.**
Date: 2026-10-27. This document is the single repository source of truth for
this proposal. It adds no endpoint, schema, account action, email delivery,
configuration, or authority. Approval to write or merge this static spec is
not approval to build or operate it. All unimplemented contracts below are
requirements for a later, separately admitted change, not claims of live behavior.

## 1. Intent, boundaries, and choice

Let someone with an existing account passkey replace an inaccessible email
without losing their library, provider vault, billing identity, or passkey.
Email remains the sole magic-link address; there is never a second active
magic-link alias. Preserve the existing post-sign-in landing contract rather
than adding a new return-path mechanism here.

**Recommended smallest safe first model:** pending candidate → prove the new
inbox → explicit passkey-authorized promotion → notify the previous inbox.
Keep the current address active until the entire promotion commits. A live
session alone, even a recently created one, is not sufficient authority.

Choices made by this design, not already approved product decisions:

- First release requires an already-enrolled passkey, for both beginning and
  finishing a change. Reuse the existing assertion verifier, not a new sign-in
  method. Require a purpose-bound assertion for this operation. An ordinary
  fresh magic-link session does not establish passkey proof.
- Magic-only accounts, or people who lost both inbox and passkey, are **NO-GO
  for self-service change**. Keep their current account usable; do not send
  them into a dead end or invent an owner-support identity override. A future
  old-inbox confirmation alternative needs its own security disposition.
- Preserve passkey credentials and their stable user binding. Revoke all
  sessions (including the initiating one), outstanding sign-in links and
  auth challenges, and native REST credentials at successful promotion or
  reversion. Mint no replacement session as a side effect. Then sign in
  normally with the retained passkey or the now-current email.
- A **24-hour verified reversion window** retains the old address as an
  unavailable-for-registration claim, not as a login alias. Reversion needs
  both that inbox's new purpose-bound proof and a retained account passkey.
  An old-inbox link alone never signs in, changes email, freezes an account,
  revokes credentials, or grants recovery authority.
- No change to general session lifetimes, existing session policy, passkey
  management, account deletion, or sign-in methods. The mandatory revoke-all
  behavior is confined to the identity transition. No build cards are created
  by this spec.

This is deliberately narrower than general account recovery. GitHub's
[add, verify, then promote pattern](https://docs.github.com/en/account-and-profile/how-tos/email-preferences/changing-your-primary-email-address)
is prior art, not evidence that NNTune already supports multiple addresses.

## 2. Source-verified baseline and gaps

Source snapshot: `0052c8434924b267f3c7a998312f9051e9779418`. Citations were
verified against that snapshot and remain subject to the normal citation lint;
check named handlers again before implementation.

| Current fact | Evidence / consequence |
| --- | --- |
| Email normalization trims, lowercases, bounds to 3–254 bytes, rejects whitespace/control characters and malformed `@`/domain shapes. | `workers/api/src/handlers.rs:1138-1151`. Reuse exactly; do not add plus/dot equivalence or silently merge accounts. |
| Magic proof is a 256-bit random token, stored as a SHA-256 hash with user ID and email; lifetime 900 seconds. Pure validation is not an atomic spend. | `workers/api/src/auth.rs:17-20,70-110,113-151`. A new-purpose token cannot be redeemed by sign-in or registration. |
| Known, unknown and issuance-limited magic starts answer the same admitted `200 {"ok":true}`; no delivery guarantee. | `workers/api/src/handlers.rs:2243-2269`. Do not turn candidate uniqueness or send suppression into an email-existence oracle. |
| Issuance budgets are shared-store reservations: auth starts 30/IP/hour plus configured burst (default 10/60 seconds); mail 30/IP/hour and 100/global/300 seconds; 3/address/900 seconds shared by registration and magic sign-in. | `workers/api/src/handlers.rs:54-61,100-111,1287-1405`; `workers/api/src/rate_limit.rs:9-40`. Pure token-bucket helpers are reference models, not the cross-isolate authority. |
| Magic finish requires exact Origin, bounds token size, throttles before lookup, rejects swapping a valid other-account session without spending the token, and only then atomically claims proof and starts a session. | `workers/api/src/handlers.rs:2272-2356`. It does not check an email-change version today; its claim and session insert are separate stages. |
| Privileged freshness is session age, not proof-method provenance. Reauth start/finish bind the challenge and credential to the session's user; verified passkey proof conditionally replaces the old session row. | `workers/api/src/handlers.rs:1262-1285,1938-1986,2102-2240`. Never infer “passkey-authorized change” merely from a fresh cookie. |
| Account security is client-rendered, not authenticated SSR. Its read loader uses `/v1/vault` as a session check; confirmed 401 clears cached identity/library state, transport failure is a warning. Current UI only signs out locally/everywhere. | `apps/web/src/routes/account/security/+page.ts:8-26`; `apps/web/src/routes/account/security/+page.svelte:18-30,68-101,110-134`. No server-rendered identity or change-email UI exists there. |
| Magic landing scrubs the URL and waits for an explicit click; it never auto-redeems on mount. | `apps/web/src/routes/auth/magic/+page.svelte:38-59`. Keep this anti-login-CSRF property; a change-email GET is also inert. |
| Current mail boundary supports magic-link payloads only. The payload includes a raw token/code; mail failure/deadline does not change admitted start success. | `workers/api/src/worker_entry.rs:392-403,1944-1978`; `workers/api/src/handlers.rs:1385-1404`. Security notices, verified delivery, and durable retry are NOT existing capabilities. |
| Binding policy requires fresh passkey OR old-inbox confirmation, old-address notice plus revert, and revocation of all sessions/outstanding magic links. Native credentials must be revoked at the same recovery event. | [Threat model §4](../security/threat-model.md#4-sessions-passkeys-magic-links-stride); [ADR 0010](../adr/0010-native-rest-credentials.md#issuance-expiry-revocation). This proposal does not waive these rules. |
| Current HTTP revoke-all revokes native credentials first, then sessions in separate store calls, and can fail between them. It does not atomically promote email or invalidate outstanding magic links. | `workers/api/src/handlers.rs:2441-2475`. Reusing this handler sequentially is insufficient for an identity transaction. |

Identity/vault binding evidence is completed in §3. Static research found no
email-change route in the current dispatch table (`workers/api/src/handlers.rs`,
`handle`). A comment anticipating recovery is not an implemented feature.

## 3. Stable identity and preservation invariant

The stable account ID, never the email string, must remain the owner of every
library, vault, credential, entitlement and usage row. Promotion updates only
the primary email claim and identity-transition metadata; it must never create
another user, merge accounts, re-key a vault, or transfer another user's rows.
Existing encrypted vault bytes, wrapped DEK/KEK version, ownership IDs and
passkey public keys must be byte-identical before and after the transition.
The implementation review must enumerate any external email-keyed bindings
before enabling this feature; an unverified binding is a rollout NO-GO.

**Required evidence before build:** stable user ID/schema, session claims,
WebAuthn owner binding, vault AAD, and all recovery-hook call sites. See the
source map above and the binding appendix at the end of this document.

## 4. State machine, time, proof, and concurrency

### Record and clock contract (proposed, not a migration)

One active pending operation per stable user. Store an opaque random operation
ID, current identity version, old-primary snapshot, normalized candidate,
creation/expiry timestamps, candidate-proof hash and generation, state,
proof-consumed time, completion result and bounded idempotency metadata.
Never store a raw proof token, credential, or passkey assertion in the record.
Use the primary database clock for issue/expiry decisions, not an edge clock.
Capture now after acquiring all locks, at the final conditional state transition
(the linearization point); valid interval is `issued_at <= now < expires_at`.
No skew tolerance, future-issued proof or exact-deadline acceptance. A commit
acknowledgement may arrive later; validity refers to that serialized transition,
not an unknowable client-observed commit time. Lock waits do not freeze an
old transaction-start timestamp.

Candidate inbox proof: 32 cryptographic-random bytes, base64url code; hash at
rest; purpose `email_change_new`; bound to user, operation, candidate,
identity version and generation. It expires after **900 seconds**, never later
than the operation's **900-second** hard deadline. Resending rotates the token
and invalidates the old generation without extending that deadline. A new
operation after expiry requires a new begin assertion. Wrong, unknown, used,
expired, denied and wrong-purpose tokens give the same public rejection.

Every begin and every final commit requires an explicit WebAuthn assertion
using an existing credential owned by this stable user, exact RP/origin, user
verification and monotonic-counter rules. Its single-use challenge expires
in **300 seconds**, has an email-change purpose, and binds the candidate,
operation ID, identity version and current authenticated session ID. Final
commit always requests a new assertion; do not reuse the begin assertion.
Keep the existing 600-second privileged-session gate as well as these stronger
operation proofs. If stale, complete the existing generic passkey reauth first;
its challenge is not email-change authority. A replacement session needs a new
bound operation challenge, never transfer the previous challenge silently.
The pending operation belongs to the stable user, not exclusively to the begin
session: another live session of that SAME user may complete it only with the
candidate code and its own new final assertion. Check the completing session
ID equals the final challenge's bound ID at the serialized transition; a dead
begin session does not authorize anything. Removed credentials are rejected
with generic cannot-complete; credential removal and deletion must participate
in the same lock/version fence. No auto-enrollment to bypass missing proof.

### Durable states and public presentation

| Durable state | Allowed transition / result | User-facing state |
| --- | --- | --- |
| No active request | Authenticated begin with valid passkey assertion and bounds creates pending; old primary unchanged. | Idle → working → check inbox. |
| `pending` | Matching candidate proof AND final passkey assertion in one transaction → `verified`; same-user cancel → `denied`; deadline → `expired`. | Pending; current email still works. |
| `verified` | Terminal forward commit: new primary active, revocations committed, previous address held for 24 hours, notices queued. Not a “proof checked, later promote” intermediate. | Changed; sign in again. |
| `expired` | Terminal; no primary change, no revocation. Fresh begin may create a different operation. | Expired; start again. |
| `denied` | Terminal cancel, supersession, conflicting target, version mismatch or rejected authorization; no primary change. Keep internal reason private. | Cannot complete, or cancelled if the owner explicitly cancelled. |
| `reverted` | Terminal successful symmetric revert of a verified operation; all identity revocations repeated. | Previous email restored; sign in again. |

Rejected requests without valid operation authority do NOT transition a
record to denied: a stranger cannot cancel someone else's pending request.
Wrong proofs neither spend the current proof nor reveal its status. A valid
but occupied target may have a privately denied operation while the public
start remains indistinguishable from pending. Public reads must not reveal
target occupancy. At finish, it gets the same generic cannot-complete result
as expired/invalid proof. No account-link suggestion or “already registered”.

A resend/new-candidate operation is explicit, never automatic. Changing the
candidate cancels the old pending operation only after a newly authorized
begin commits; an ambiguous failure must not silently discard the known one.

### Promotion transaction and issuance fence

Under one database transaction, serialize on the stable user and unique
address claims. Re-read live session/revocation, identity version, pending
state/deadline, both proofs, credential owner and target availability. Consume
proofs and passkey counter conditionally; claim the new primary, hold the old
address for reversion, advance identity version, mark verified, revoke ALL
sessions/native credentials/outstanding magic links/auth challenges for this
user, plus any Subsonic clients (see binding appendix), record audit facts
and enqueue BOTH old-address and new-address notices. Commit everything or none of it. Do not call the existing sequential
HTTP revoke-all path as a substitute. Preserve other users' rows.

The version/lock fence must also cover in-flight magic/registration/passkey
finishes, session creation/refresh/reauthentication and native-key issuance.
A stale pre-change login snapshot must not insert a new session after the
sweep. A pre-change registration proof must not enroll a passkey after it.
Reject old-version work at its commit point under the same serialization
contract; new-primary proofs and post-change passkey ceremonies work normally.
All relevant issuance callers must participate before rollout, not merely
check version before beginning network/verification work. Old-email signup
and the 24-hour claim release must share the uniqueness fence as well.

A final assertion does not revive a dead completing session. If that session
was revoked before the transition lock is acquired, reject without promotion.
For proof races, only one commit wins. Use one documented deterministic lock
order across issuance, deletion, begin, resend, finish and claim-release:
user IDs sorted, then address claims sorted, then operation/proof rows. Resend
and finish serialize: finish-first makes resend a no-op; resend-first makes
the old generation fail. Concurrent distinct begins serialize, the later one
supersedes the earlier pending request, and either stale final proof loses.
Idempotency is scoped to user + action + key and binds a canonical body hash;
same key/different candidate is 409 input_conflict (never an occupancy signal).
A retry reads the saved result and sends no extra mail. Assertion counter and
challenge/code consumption are in the SAME transaction as their authorized
begin/finish, including rollback: none advance on rollback, all do on commit.
An assertion in a rolled-back attempt can retry only within its original TTL
and binding; a committed one cannot be replayed. A lock timeout before any
mutation returns proven not_changed; uncertain commit acknowledgement returns
outcome_unknown. Never claim a rollback merely from a generic timeout.

## 5. Proposed surface, no-oracle limits, and unknown outcomes

These are **proposed API names only**; none may be called in this design run:

| Operation | Authority and effect |
| --- | --- |
| `GET /v1/account/email-change` | Valid session; read only own redacted status. No email/candidate from browser storage is authority. No target availability signal. |
| `POST /v1/account/email-change/begin` | Same-origin session + purpose-bound begin assertion, bounded candidate and idempotency key. Generic accepted pending envelope for valid-shaped occupied/free candidate. |
| `POST /v1/account/email-change/resend` | Same-origin owner session + operation; rotates only after cooldown/admission, never promotes. No target existence or mail-delivery signal. |
| `POST /v1/account/email-change/finish` | Same-origin owner session + new-inbox proof + new purpose-bound final assertion. Atomic transaction above; no bearer/native API-key path. |
| `POST /v1/account/email-change/cancel` | Same-origin live owner session, own operation and version; idempotent, never undoes a committed promotion. |
| `POST /v1/account/email-change/revert/begin` and `/finish` | Same user, retained passkey, purpose-bound old-inbox proof and fresh revert assertion; §6 window/uniqueness rules. |

Challenge begin/finish plumbing for assertions may share existing verifier
internals but must use distinct purpose/bindings; endpoint spelling is left
to the future builder. No GET, link preview or mail scanner spends a token.
Verification uses a code entered in the authenticated account page; the new
email contains no auto-sign-in link. All state/error reads are private,
`Cache-Control: no-store`; responses do not echo tokens, other identities,
email existence, internal denial reasons, or delivery receipts.

**Proposed additional durable budgets:** one active request/user; 60-second
resend cooldown; at most 3 begin/resend issuance attempts/user/900 seconds
and 3/normalized-target/900 seconds, shared with ordinary auth's address
budget (no new mailbox-bombing allowance). Also consume existing mail IP and
global budgets; security notices need explicitly reserved bounded capacity,
not an unlimited bypass. Finish/cancel/status: at most 10/user/300 seconds
and 100/IP-prefix/300 seconds; proof lookups additionally 10/operation/300
seconds, all before expensive work where the key is available. Anonymous or
wrong-owner attempts remain generic. Cooldown/user throttles may return 429
with a whole-second Retry-After independent of target occupancy. Suppression
by target/global mail budget must retain the same admitted envelope for
occupied/free targets. Reuse the SAME normalized-target reservation counter,
not a parallel equal-sized quota: auth and change can consume each other's
three sends; that existing-availability tradeoff needs the security disposition.
IP-prefix means IPv4 full address or IPv6 /64, exactly as current auth. Any
anonymous/dead-session request is generically 401 before target/operation
lookup; wrong-owner and nonexistent operation IDs are identically rejected
for a live session. During begin/resend, store/entropy suppression preserves
the admitted envelope on every target class, creates no usable proof and
sends nothing; during final commit proven rollback uses not_changed instead.
No valid-looking proof is ever fabricated. All budgets are shared across isolates and fail closed; no
client-only limit, unbounded scan, timer retry, or mailbox-hash logging.

Use current normalization, reject malformed input locally without an account
lookup, and reject unknown fields. Proposed operation/idempotency/proof IDs
are bounded ASCII opaque strings (maximum 128/128/256 bytes respectively);
body stays under the existing 32 KiB ceiling. Do not add speculative email
provider canonicalization. Server-rendered HTML must contain no identity or
proof; keep the existing client-rendered account boundary.

Public fixture shape is compact and distinct from existing live API errors:
`{state, error_code, next_action, retry_after_seconds?}`. Future API-error
integration must preserve existing generic 401 and `reauth_required` semantics;
never treat this proposed shape as already deployed. Proposed HTTP mapping:
200 for admitted start/resend/status/idempotent terminal result; 400 for local
malformed input or the single cannot_complete proof envelope; generic existing
401 for dead/absent session, distinct 401 reauth_required for a live session
missing freshness/proof; 429 + Retry-After for owner-known limits; 409 only
for idempotency-body conflict. Uncertain 5xx/transport failure maps to the
client-only outcome_unknown; not_changed is only an authoritative 503 after
proven rollback. Never use 404 vs 200 to reveal operation-ID existence.

GET returns only the authenticated user's current primary email and latest
operation receipt (within 48 hours): opaque operation ID, public state,
error_code, next_action. Candidate value, target-existence reason, proof
hash/generation and delivery outcome are excluded. An internally occupied
target is presented as pending until the same public deadline, then expired;
finish still rejects generically. Public expiry/resend availability is based
only on admission time/owner budget, never target uniqueness. An absent latest
operation is idle. An optional supplied unknown or wrong-owner operation ID
gets the same 400 cannot_complete without affecting any real operation.
A freshly authenticated same-user session can rediscover its latest receipt
without an old tab/operation ID; the 24-hour forward-change hold guarantees no
second completed forward change inside the 48-hour result window's first day.
After that, latest means newest creation order; older IDs remain owner-readable
for their own 48-hour retention. No unauthenticated receipt lookup or listing
of another user's transitions exists.

| Observation | error_code / next_action | Truthful behavior |
| --- | --- | --- |
| Admitted start/resend | `null` / `check_inbox` | “If this address can receive the verification message, check its inbox.” Not “Email sent”. |
| Live but insufficient proof | `reauth_required` / `use_passkey` | Explain the required passkey; do not fall back to a new-address magic login. |
| Dead session | `unauthenticated` / `sign_in` | No change confirmed; preserve distinction from a successful revoke-all. |
| Invalid/expired/used/occupied/wrong-purpose proof | `cannot_complete` / `start_again` | Same status/body for these causes, no target hint. Owner-authenticated known expiry may show expiry without occupancy information. |
| User/cooldown limit | `rate_limited` / `wait` | Retry-After; no automatic resend. |
| Verified transaction success | `null` / `sign_in` | Clear cookie and local identity/library snapshots; broadcast auth transition; no credentials in storage. |
| Proven rollback before commit | `not_changed` / `retry_explicitly` | “Your email was not changed. Try again.” Only after authoritative rollback evidence. |
| Timeout, lost response, ambiguous 5xx or uncertain commit | `outcome_unknown` / `check_status` | “We could not confirm whether your email changed. Check status before trying again.” Never declare rollback or success. |

For an ambiguous finish, do not mint a second request or automatically retry
a mutation. A live same-user session may read status. If promotion killed the
session, sign in with the retained passkey, then read the durable result
using the original idempotency/operation ID. A 401 alone proves no commit
outcome. Keep only a nonsecret operation ID in tab storage, not addresses or
proofs. If the result cannot be recovered, remain unknown, offer normal
sign-in, and never guess based on email arrival. Record idempotent terminal
results for **48 hours**; beyond that say “Result unavailable; sign in and
check your current email”, not “not changed”. No auth from a receipt token.

## 6. Notification, secure-my-account, and verified revert

Promotion atomically enqueues old- and new-address security notices with a
unique event ID. Provider acceptance is not inbox delivery; timeout is unknown.
Do not roll back a completed identity change because mail is delayed. Use a
transactional outbox and idempotent delivery keyed by event ID; no new host
script/timer. Proposed bounded retries: immediate, then 1 min, 5 min, 30 min,
2 h and 6 h; stop at 24 h, surface a names-only operational delivery alert.
A delivery failure never exposes recipient or proof in telemetry. Host/email
executor must confirm existing sanctioned worker/outbox support before build.
The outbox row insert is inside the identity transaction; insertion failure
rolls back identity/proofs/revocations. Provider delivery is strictly after
commit; provider failure never rolls them back. Audit rows in that same
transaction contain stable user/operation ID, action, server time and result,
not email, IP or proof; do not reuse the existing best-effort audit helper
for the authoritative transition. Retain operation receipts 48 hours; minimize
address data to the active request/24-hour hold and necessary notice delivery,
then purge it under the existing privacy retention policy. Longer audit
retention follows that policy without retaining old addresses/proof hashes.
This outbox carries notice data only; proof codes are hashed in the operation
record and delivered inline after commit, never persisted raw in an outbox.
A failed/unknown proof send can be explicitly resent under the limits above.

Old-address copy:

> Your NNTune sign-in email changed at {UTC time}. All sessions and native API
> keys were revoked. Your library and saved provider settings were not moved.
> If this was you, sign in with your passkey or new email. If it was not you,
> open NNTune directly and choose Secure my account. Restoring the previous
> email requires your account passkey and access to this inbox within 24 hours.
> This message does not contain a sign-in code or authorize a change.

New-address copy: “Your NNTune sign-in email is now this address. Sign in again
with your passkey or request a new sign-in email. Previous sessions and native
API keys no longer work.” No provider details, raw IP, API-key prefix or full
previous/candidate email appears in either notice. Never tell someone to
reply with a secret. Notice links use a configured trusted application origin
and a fixed inert account-security path; no recipient/token in URL or referrer.

“Secure my account” first signs in normally with a retained passkey and shows
owner-only transition status. It does not auto-login via the notification.
Within `committed_at <= now < committed_at + 86400`, offer **Restore previous
email**. Begin requires a purpose-bound passkey assertion and sends a separate
900-second, single-use `email_change_revert` proof to the old inbox, bounded
by the 24-hour deadline and the same mail limits. Finish requires that code
and another fresh purpose-bound passkey assertion bound to operation/current
identity version/session. Apply the same promotion transaction and issuance
fence, restore the held address, and notify both inboxes. It revokes every
session and native key again; passkeys/vault stay intact.

Only the latest verified change is revertible. Reject if the identity version
advanced, deletion is pending, the 24-hour window expired or the reserved
claim is unavailable; generic cannot-complete, never overwrite a third party.
No chained automatic reverts. After reversion the displaced candidate is
released; a new change needs a full new ceremony. A further forward change
is not admitted during the 24-hour revert window, preventing unbounded old
address reservations. Release the old claim after expiry under the same
uniqueness lock; it is never an alias during the hold. Registration performs
lazy expiry/release under that lock on every claim attempt, so no unrelated
write or cleanup timer is needed. A held address gets the same existing
nonenumerating registration envelope as an occupied address, never a
special “recently changed” error. Old-inbox access alone cannot use magic
start to sign in to the changed account. Revert-code resend uses the same
60-second cooldown, three-per-900-second quotas, generation invalidation,
original 900-second operation deadline and 24-hour cap as forward proof.

**Residual boundary requiring explicit security acceptance:** an attacker
already controlling the same account passkey and old inbox can satisfy the
revert proof. A user with neither a retained passkey nor current inbox has
no self-service recovery in this model. Do not promise guaranteed recovery,
introduce account freeze, or remove passkeys under this card.

## 7. UI, keyboard, narrow screens, and assistive technology

Proposed entry: Account security → “Change sign-in email”. Initially load the
authenticated server identity; never render the stored signed-in email label
as authority. Unknown session status shows “Could not verify your account.
Try again” with no enabled mutation. No passkey: explain the limitation,
keep existing security actions accessible, and expose no unusable form.

| UI state | Copy and controls | Focus / announcement |
| --- | --- | --- |
| Idle | Current email (owner only); labelled “New sign-in email”; helper “Your current email works until the change completes”; Continue, Cancel. | Normal heading/label order. Invalid format associates a single inline error using aria-describedby/aria-invalid, then focuses the field. |
| Begin/verify busy | “Confirm with your passkey” / “Checking…”; disable only conflicting controls and expose aria-busy. | Keep current focus stable; no competing modal or surprise navigation. |
| Pending | “Check your new inbox. Your current email has not changed.”; labelled Verification code; “Verify and change email”; Resend; Cancel change. | Focus the pending heading after branch entry, then code field by Tab; one polite status announcement, no per-second countdown announcements. |
| Passkey cancelled/denied | “Passkey confirmation did not finish. Your email has not changed.” only if no commit was attempted; Try passkey again, Cancel. | Inline alert once; preserve typed candidate/code in component memory while mounted; focus retry when the branch changes. |
| Expired/denied | “This request cannot be completed. Start again. Your current email is unchanged.” only with confirmed noncommit state. | Focus result heading; no automatic new request or resend. |
| Unknown | “We could not confirm whether your email changed.”; Check status; Sign in with passkey if session ended. | Focus unknown heading, polite status; disable duplicate mutation, never a false success toast. |
| Verified/reverted | “Email changed. Sign in again.” / “Previous email restored. Sign in again.” | Clear cross-tab identity/library snapshots on confirmed boundary even if route was abandoned; navigate only from a live view to existing sign-in. |
| Revert expired | “The restore window has ended. Your current email is still active.” | Read-only result; existing sign-in/security links remain usable. |

Escape cancels only a not-yet-submitted local dialog, never a server mutation.
Leaving a pending request preserves server state until expiry/cancel; leaving
an in-flight finish suppresses late focus/navigation but does not discard its
result or claim rollback. Resend outcomes remain adjacent to the resend
control and do not steal code-field focus. No window.confirm-only flow.

Acceptance at 320 CSS px and 200% zoom, in light and dark: single-column form,
wrapping email strings, no horizontal scroll, visible focus, no clipped text,
minimum 24-by-24 CSS-pixel targets or WCAG spacing exception, normal text
contrast ≥4.5:1 and control/focus contrast ≥3:1. Status never depends on color.
Screen-reader acceptance: labelled main/heading/inputs, all errors linked,
branch-entry heading focus only, one status/alert per outcome, no unexpected
assertive duplicate from the form, no announced raw code/token, no disabled
control focus trap, full keyboard completion/cancel/resend/recovery. Evaluate
form announcements separately from the application's existing route announcer.

## 8. Threat model and hard rejects

| Threat | Required containment / case |
| --- | --- |
| Stolen session changes email for persistence | Begin AND finish purpose-bound passkey proof; fresh cookie is not sufficient; exact Origin; transaction rechecks current session. EML-02/03/09. |
| Attacker supplies their own inbox or link; login-CSRF | New code never authenticates, GET inert, explicit owner confirmation, owner/version/purpose binding; no auto-redemption or cross-account swap. EML-04/05. |
| Old inbox compromised after change | No alias, no automatic revert or login authority; passkey plus old-inbox proof for narrow revert. EML-13/14. |
| Session fixation / pre-change proof races sweep | Revoke all; fence every issuance commit by identity version/serialization; preserve passkeys but invalidate old challenges. EML-07/08/10. |
| Enumeration, address squatting, resend bombing | Indistinguishable candidate-existence envelopes, no free/occupied hint at status; no pending address reservation; bounded shared budgets; old claim hold capped at 24 h. EML-06/11/18. |
| Partial email swap, vault loss or account merge | One transaction; stable user and vault bytes; occupied claim never stolen; failure injection at each boundary. EML-01/07/12. |
| False success/rollback after transport loss | Idempotency and durable owner-only read; unknown remains unknown; no auto-retry. EML-15/16. |
| Security notice lost / mail scanner spends authority | Atomic notice enqueue, bounded retry and explicit nondelivery state; notification itself inert, code never in query/history/logs. EML-05/17. |

An implementation that cannot meet ANY containment is NO-GO, not permission
to ship an insecure fallback. Existing general login remains unchanged except
for the required identity-specific stale-proof issuance fence.

## 9. Offline fixtures and stable acceptance cases

All addresses below are synthetic `example.test`; IDs are inert fixture
labels. No real mail, login, deployed request, database mutation, key staging,
or browser against a real account is needed to review this design. Fixture
rows are proposed input/output data, not executable acceptance tests already
present in this PR. The Test Author owns the future acceptance banks.

| ID | Offline setup / event | Expected fixture `state / error_code / next_action` and invariant |
| --- | --- | --- |
| EML-01 | `user_a`, old `old@example.test`, candidate `new@example.test`; valid inbox proof + final passkey. | `verified / null / sign_in`; same user ID/vault bytes/library/billing owner/passkeys; only primary changes. |
| EML-02 | Fresh cookie created by magic, no operation passkey assertion. | `pending / reauth_required / use_passkey` (UI intent only, no durable request yet); zero promotion/mail without begin proof. |
| EML-03 | Wrong-user credential, wrong RP/origin, expired challenge or reused begin assertion. | `denied / cannot_complete / start_again`; no request/proof spend or session mint. |
| EML-04 | Candidate code presented to ordinary magic finish/register; login code presented to change finish. | `denied / cannot_complete / start_again`; zero authority across purposes. |
| EML-05 | Open notice/new-email message, GET reload, link scanner; form never submitted. | `pending / null / check_inbox`; zero consume/promote/sign-in. |
| EML-06 | Free/occupied candidate pair, unknown/malformed token pair; identical admitted status/body and bounded lookup path. | `pending / null / check_inbox` at begin; `denied / cannot_complete / start_again` on invalid finish; no occupancy hint. |
| EML-07 | Two finishes, finish vs cancel/resend/superseding begin/deletion, concurrent distinct begins, two users racing same candidate, same idempotency key/different body. | Serialized winner and generation/version checks; no promotion on deletion; input_conflict only for body mismatch; uniqueness and all-or-none revocation. |
| EML-08 | Old magic/register/passkey finish/session-refresh/native-key issuance paused before commit; promotion commits first. | `denied / cannot_complete / start_again`; no post-sweep session/credential from an old version. |
| EML-09 | Session revoked or freshness/auth proof absent while finish is paused; bearer key substituted. | `pending / unauthenticated / sign_in`; old primary stays; bearer never authorizes. |
| EML-10 | Two other sessions and outstanding magic/challenges/native keys at promotion. | `verified / null / sign_in`; ALL invalid on next use; a new passkey sign-in works; no replacement cookie. |
| EML-11 | 60-second cooldown boundary, fourth issuance in 900 s, rotated IPs within IPv6 /64, target/global exhaustion, store/entropy failure. | `pending / rate_limited / wait` for user-known throttle; admitted generic otherwise; no excess send/row; rotated token dead; deadline not extended. |
| EML-12 | Inject rollback at claim/proof/counter/revoke/audit/outbox/commit boundaries. | `pending / not_changed / retry_explicitly` only for proven rollback; old primary/proofs/sessions/counter/vault unchanged together. |
| EML-13 | Old-inbox-only attacker requests old magic login or follows Secure my account. | `denied / cannot_complete / start_again`; no new session, email change, freeze or revocation. |
| EML-14 | Revert at 86399 s with old-inbox + passkey; at 86400 s; wrong version; conflicting/absent hold. | First `reverted / null / sign_in`; other cases `denied / cannot_complete / start_again`; no third-party overwrite or chained revert. |
| EML-15 | Commit succeeds, response dropped; stale session returns 401; same-key retry after passkey sign-in, then rediscovery from a fresh tab with no operation ID. | First `unknown / outcome_unknown / check_status`; then owner-only durable `verified / null / sign_in` result, no second side effect or unauthenticated lookup. |
| EML-16 | UI unmounts during success/rollback/unknown; other tabs cache old identity/library. | No late navigation/focus. Confirmed boundary clears/broadcasts even after unmount; unknown does not show success or promote cached candidate. |
| EML-17 | Notice provider rejects/times out/duplicates; notice outbox insert fails. | Verified result stays verified after delivery failure; bounded retry/alert; failed enqueue rolls back transaction; no raw proof/mail address in telemetry. |
| EML-18 | Code at now=expiry−1, expiry, future-issued; cancelled/superseded token; fourth change inside revert window. | Strict expiry; stale generations rejected; new forward change denied during hold, unchanged primary and bounded claims. |
| EML-19 | Keyboard-only, screen reader, 320 px/200% zoom, light/dark, passkey dismiss, expiry and transport failure. | §7 focus, announcement, contrast and copy contract; no trap, horizontal scroll, auto-send or false outcome. |

Proposed executable checks **after admission**, with test filenames to be
created by the Test Author (these commands are not claimed runnable today):

```sh
# Pure handlers + real transaction adapter, synthetic fixtures and fault barriers:
python3 scripts/cargo_cache.py run -- test --manifest-path workers/api/Cargo.toml --test verified_email_change_acceptance
# Offline intercepted HTTP browser states, no external mail or account:
pnpm --dir apps/web exec playwright test tests/verified-email-change.spec.ts --workers=1
```

The Rust bank must assert affected-row counts, uniqueness, byte-preservation,
and commit order, not just source strings; race tests use deterministic
barriers. The SQL/adapter bank must use only the authorized isolated test
database and prove rollback against real constraints; mocked Store success
is not transaction evidence. Browser interception must record methods/bodies
and zero external sends, and exercise all EML rows that affect presentation.

Current regression banks to keep unmodified include
`workers/api/tests/privilege_reauth_acceptance.rs`,
`workers/api/tests/magic_link_window_cap_acceptance.rs`,
`workers/api/tests/auth_throttle_envelope.rs`,
`workers/api/tests/scoped_native_rest_credentials_acceptance.rs`,
`apps/web/tests/auth-magic.spec.ts`,
`apps/web/tests/auth-magic-retryable.spec.ts`,
`apps/web/tests/account-security.spec.ts`, and
`apps/web/tests/account-security-library-cache.spec.ts`.
They are baseline context, not coverage for this unimplemented feature.

## 10. Explicit dispositions and future slices — no task creation

**Current decision: NO-GO for implementation, rollout, real sends or identity
mutation.** Static spec merge does not change that. Before any build:

1. **CISO design disposition** bound to this spec revision/commit: approve,
   approve-with-enumerated-conditions, or HOLD. Explicitly cover passkey-only
   eligibility, operation-purpose/UV proof, generation and issuance fences,
   24-hour old-address claim/revert model, mailbox-compromise residual,
   no-oracle budgets, strict TTL boundaries, audit/outbox atomicity,
   revocation completeness (including any non-native credential class), and
   unknown-result recovery. This does not supersede the current threat model;
   incompatible wording or behavior requires an explicit policy amendment.
2. **CEO design disposition** on the same revision after security conditions
   are resolved: accept/defer/reject product scope, magic-only exclusion,
   recovery limitation, 24-hour change cooldown, user copy and offline
   acceptance plan. No owner approval hop; no automatic build admission.
3. **Host/email alignment** by the authorized executor: attest actual
   transactional store constraints/locks and every issuance caller; outbox
   delivery/retry mechanism, shared quotas and alert ownership; synthetic
   staging-only validation and rollback (disable new feature entry without
   undoing verified identities); no new spend, credentials, production tests
   or migrations under this design card. Missing capabilities mean HOLD,
   not sequential best-effort identity writes.
4. Later implementation retains same-head CI, independent security/review
   gates, staging evidence and authorized deployment contract. No disposition
   here authorizes passkey management, old-inbox-only recovery or session
   policy redesign.

Candidate future slices, each **at most one engineering day**, only if the
above dispositions explicitly admit them; do not create these cards now:

| Slice | Deliverable / bound | Depends on |
| --- | --- | --- |
| A | Test Author freezes EML-01–19 offline acceptance banks and failure barriers; no product activation. | Dispositions |
| B | Inert transition/unique-claim/outbox schema and transactional adapter with rollback/uniqueness proofs. No live route. | A, host alignment |
| C | Identity-version issuance fence at every auth/session/native-key commit point; regression/race evidence. | B |
| D | Purpose-bound passkey begin + bounded candidate proof issuance/resend/cancel, held inactive. | B, C |
| E | Atomic forward promotion/revocation/audit/notices and durable unknown-outcome read, held inactive. | C, D |
| F | Verified revert and old-claim expiry using the same transaction contract, held inactive. | E |
| G | Account UI, focus/copy/cache-boundary integration and intercepted browser cases; no enabled production entry. | D–F |
| H | Executor-owned synthetic staging validation, notice retry/health/rollback evidence and separately authorized activation. | All acceptance + review gates |

If any slice cannot fit one day, return NO-GO and narrow/re-spec it before
admission; never ship forward promotion without the required revoke fence,
notice path and safe revert simply to meet the timebox. No operational
account recovery is performed or promised by completing this document.

## Binding appendix

| Binding / hook | Source-verified behavior |
| --- | --- |
| User and unique email | `workers/api/src/handlers.rs:393-398,1550-1566` separates opaque user ID from email; `migrations/0002_auth_vault.sql:9-25` gives unique email and user-ID-owned passkeys. Normalization is application-level, not a database canonicalization constraint. |
| Sessions | `workers/api/src/session.rs:67-79,316-346` contains user/session ID, not email, and checks the live stored row. `workers/api/src/handlers.rs:1231-1284` performs the row lookup on authenticated requests. Native credentials also consult their own live record, independently of sessions (`workers/api/src/handlers.rs:3116-3161`). These are not stateless-only authenticators. |
| Passkeys | `apps/web/src/lib/webauthn.ts:65-85,95-103` uses stable ID bytes as user handle and email only as name/displayName. `workers/api/src/handlers.rs:2046-2064,2133-2168` checks stored credential owner against challenge/session owner. Authenticator-visible old-email labels may remain; changing them is out of scope, and UI must explain that they still identify the same passkey. |
| Vault | `migrations/0002_auth_vault.sql:62-75` keys vault by user ID; `workers/api/src/vault.rs:541-547,555-597,603-649` binds DEK wrap to user ID plus KEK version, payload to user ID only. No email-derived key or AAD. Preserve ciphertext/nonces/wrapped DEK/version; no KEK rotation for an email change. |
| Native REST | `migrations/0019_native_rest_credentials.sql:18-35` keys owner by user ID. `workers/api/src/native_rest.rs:376-396` is a pure in-memory helper, not an installed email-change database hook. Production sweeps use Store calls (`workers/api/src/worker_entry.rs:1162-1177,1319-1344`) from `session_revoke_all`. |
| Challenge purposes | `migrations/0013_auth_challenge_reauth_purpose.sql:6-11` currently permits only existing purposes, not email-change purposes. Adding operation binding is a future reviewed schema change, not already-supported plumbing. |
| Account deletion | `migrations/0006_account_deletion.sql:7-8` adds deleting_at; current vault writes fence concurrent deletion (`workers/api/src/worker_entry.rs:1404-1440`). Both forward and revert must serialize with deletion and reject pending deletion without consuming proof. |
| Subsonic credential gap | `migrations/0023_subsonic_clients.sql:20-43` defines user-ID-owned per-client keys and revocation. [Subsonic port status](../adr/0001-subsonic-api-port.md#decision) says the current Worker shell has no accepted credentials until the persistent adapter lands. Do NOT infer that session/native sweeps cover these rows. Future email transition must revoke/fence every Subsonic client row too if present, including legacy-secret use, before activation. The host/security disposition must verify the then-current adapter rather than assuming today's inert shell. |

All binding findings above are static source evidence, not runtime or database
inspection. External billing/email-provider bindings, active deployment state
and real delivery were not verified. Their alignment is explicitly a rollout
NO-GO until the authorized executors provide evidence.
