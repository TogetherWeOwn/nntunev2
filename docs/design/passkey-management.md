# Passkey inventory, add and remove: static specification

**Status: proposed; implementation NO-GO.** This document changes no authority,
credential, route, schema, session or deployed behavior. Spec admission is not
CISO/CEO design disposition, implementation approval or permission to operate
credentials. No real ceremonies or product endpoint calls were used to write it.

Source snapshot: `0052c8434924b267f3c7a998312f9051e9779418`, inspected 2026-10-27.
This file is the single repo source of truth. Task-document and downloadable
copies are review snapshots, not independently editable contracts. Revalidate
sources and bind dispositions to a document revision before any future build.

## 1. Scope and decision

Add a **Passkeys** section to the existing `/account/security` page: private
inventory, add another passkey, individually confirmed remove. Keep sign-out
controls and their behavior intact. Do not reuse signup as authenticated add.
Do not overlap native REST credentials, change magic links/session management,
add a sign-in method, run credential operations or create implementation cards.

Minimal first version: optional label at creation, no rename, at most eight
passkeys, unknown metadata shown honestly. Last-passkey removal is unavailable
until an existing fallback method can be authoritatively proven usable and its
removal policy receives both dispositions. Seeing an email address or obtaining
a session through a magic link is not that proof. A second passkey permits
removing the first, but is not an independent fallback sign-in method.

**NO-GO today:** authenticated add/list/remove, management-bound proof, atomic
last-key enforcement, metadata and fallback verification are missing contracts.
The proposals below specify the boundary to review; none is an installed API.
No UI-only implementation may expose working mutation controls against signup.

## 2. Source-verified current behavior

Citations refer to the source snapshot above; names are the durable search keys.

### 2.1 Existing ceremonies and wire bodies

All routes below are POST. Binary values are unpadded base64url. Finish request
structs reject unknown fields. Browser credential objects are flattened by the
client; transports, attachment, extensions and userHandle are not finish fields.
Sources: `workers/api/src/handlers.rs:1416-1444`,
`apps/web/src/lib/api/client.ts:287-324`, `apps/web/src/lib/webauthn.ts:36-54`.

| Route | Request | Success |
| --- | --- | --- |
| `/v1/auth/passkey/register/start` | `{email}` | 200 `{challenge,user_id,rp_id}` |
| `/v1/auth/passkey/register/finish` | `{challenge,credential_id,attestation_object,client_data_json,magic_token,invite_code?}` | 200 `{ok:true}` and new session cookie |
| `/v1/auth/passkey/login/start` | `{email}` | 200 `{challenge,allow_credentials:[],rp_id}` |
| `/v1/auth/passkey/login/finish` | `{challenge,credential_id,authenticator_data,client_data_json,signature}` | 200 `{ok:true}` and new session cookie |
| `/v1/session/reauthenticate/start` | client sends `{}`; no route-specific body parsing | login-start shape |
| `/v1/session/reauthenticate/finish` | login-finish shape | 200 `{ok:true}` and replacement session cookie |

Sources: `workers/api/src/handlers.rs:1467-1501`,
`workers/api/src/handlers.rs:1840-1856`,
`workers/api/src/handlers.rs:1928-1986`,
`workers/api/src/handlers.rs:2083-2099`,
`workers/api/src/handlers.rs:2233-2240`,
`apps/web/src/lib/api/client.ts:471-546`.

Registration is initial enrollment only. An existing account with any passkey
receives the same-shaped stand-in/unusable challenge; finish also rejects it.
Real registration requires inbox proof belonging to the challenge's user;
optional/required invites are also validated. Proof is account-bound, not
registration-challenge-bound. The proof claims and insertion are sequential,
not a single enrollment-wide transaction. Never loosen these existing routes
to add another key. Sources: `workers/api/src/handlers.rs:1536-1546`,
`workers/api/src/handlers.rs:1725-1839`.

Login's browser ceremony is discoverable, but HTTP login remains email-selected:
finish requires the selected account's credential. Unknown-email starts have
the same public shape; no credential inventory is returned. Session reauth uses
purpose `reauth`, requires the current cookie and exact request Origin, and checks
challenge user and credential user against that cookie. Its challenge has no
session-ID binding: it is account/purpose-bound, and finish replaces the session
presented at finish, not necessarily the one used at start. Sources:
`workers/api/src/handlers.rs:1894-1935`,
`workers/api/src/handlers.rs:2018-2050`,
`workers/api/src/handlers.rs:1945-1973`,
`workers/api/src/handlers.rs:2109-2230`,
`workers/api/src/handlers.rs:415-420`.

### 2.2 RP, verifier, allow-lists and limits

- Required runtime variables are RP_ID and RP_ORIGIN. Checked-in production is
  `nntune.com` / `https://nntune.com`; staging is `staging.nntune.com` /
  `https://staging.nntune.com`. These are config-file observations, not live
  settings. RP ID must exactly equal the canonical origin host; one configured
  origin, not a multi-origin list. HTTPS except explicitly enabled loopback dev.
  Sources: `workers/api/src/worker_entry.rs:186-187`, `config/prod.json:6-7`,
  `config/staging.json:6-7`, `workers/api/src/auth.rs:185-203`.
- Verifier checks client-data type/challenge/exact origin, rejects crossOrigin,
  verifies RP-ID hash and requires UP plus UV. ES256 and EdDSA are accepted;
  attestation is none or packed self-attestation, not x5c chains. Counter 0 to 0
  is allowed; otherwise presented count must increase. Sources:
  `workers/api/src/webauthn.rs:330-388`,
  `workers/api/src/webauthn.rs:469-496`,
  `workers/api/src/webauthn.rs:524-530`.
- Signup creation requests discoverable credentials, required UV, none
  attestation and an empty excludeCredentials. Login and reauth return empty
  allow-lists. Do not infer a device/provider from these choices. Sources:
  `apps/web/src/lib/webauthn.ts:65-86`,
  `apps/web/src/lib/webauthn.ts:118-129`,
  `workers/api/src/handlers.rs:1930-1933`,
  `workers/api/src/handlers.rs:1980-1983`.
- Challenges are 32 random bytes, expire after 300 seconds and are single-use;
  finish checks a 43-character challenge ID. Inbox tokens last 900 seconds.
  Request bodies are capped at 32 KiB. Sources:
  `workers/api/src/auth.rs:17-20`, `workers/api/src/auth.rs:53-56`,
  `workers/api/src/auth.rs:205-211`,
  `workers/api/src/handlers.rs:1073-1077`,
  `workers/api/src/handlers.rs:3323-3325`.

| Existing rate scope | Limit and observable denial | Sources |
| --- | --- | --- |
| Register/login start IP | shared 30/hour plus default 10/60-second burst; register IP denial 429 with Retry-After; login preserves shaped 200 | `workers/api/src/handlers.rs:67-111`, `workers/api/src/handlers.rs:1287-1375`, `workers/api/src/handlers.rs:1503-1506`, `workers/api/src/handlers.rs:1878-1892` |
| Register/login account | separate actions, 10/hour each; opaque scope for unknown login email, ordinary-shaped 200 denial | `workers/api/src/handlers.rs:1570-1586`, `workers/api/src/handlers.rs:1894-1914` |
| Reauth start | 10/user/hour, generic 401 denial | `workers/api/src/handlers.rs:1955-1967` |
| Each finish | 100/IP/300 seconds before lookup; 10/challenge/300 seconds before crypto; generic 401 denial | `workers/api/src/handlers.rs:1678-1723`, `workers/api/src/handlers.rs:2001-2044`, `workers/api/src/handlers.rs:2119-2162` |
| Inbox mail | 30/IP/hour, 100/global/300 seconds, 3/email/900 seconds; no token/mail on denial | `workers/api/src/handlers.rs:1385-1404` |

Burst overrides have bounded validation; do not describe defaults as immutable
runtime values. Source: `workers/api/src/rate_limit.rs:206-246`.

### 2.3 Storage, errors, sessions and UI

Credential rows contain credential ID, owning user ID, COSE public key, sign
counter and registration Unix timestamp. No label, last-used timestamp,
transports, AAGUID or backup eligibility/state is persisted. The verifier
explicitly does not record BE/BS. Sources: `migrations/0002_auth_vault.sql:17-35`,
`workers/api/src/auth.rs:255-263`, `workers/api/src/webauthn.rs:21-28`.

**No passkey list, remove, rename or authenticated add route exists** in the
dispatcher. `/v1/credentials` is the separate native-REST bearer surface.
Sources: `workers/api/src/handlers.rs:3341-3375`,
`apps/web/src/lib/api/client.ts:449-467`.

The registry uses `{code,message,retryable}`, not `error_code/next_action`.
Relevant registered codes are invalid_request (400), unauthorized and
reauth_required (401), forbidden (403), not_found (404), account_deleting (409),
validation_failed (422), rate_limited (429), internal_error (500) and
service_unavailable (503); only 429/500/503 are retryable. Other registered codes
are provider_auth_expired (401) and plan_limit_exceeded (402). Unknown codes fall
back to 500 internal_error. Sources: `workers/api/src/api_error.rs:24-107`.

Reachable auth handlers also return legacy generic 401
`{error:"check your email and try again"}` and 500 `{error:"internal error"}`.
Do not promise a uniform structured envelope for old routes. Freshness failure
is structured reauth_required. The frontend validates start-success shapes
before opening a ceremony. Sources: `workers/api/src/handlers.rs:906-969`,
`apps/web/src/lib/api/client.ts:223-277`.

Session created_at is the fresh-auth timestamp: 600 seconds inclusive. Refresh
preserves it; no method-specific proof receipt is in session claims. Existing
reauth replaces only the current session, atomically revoking its predecessor
and inserting the replacement. Sources: `workers/api/src/session.rs:19-26`,
`workers/api/src/session.rs:67-79`, `workers/api/src/session.rs:358-416`,
`workers/api/src/worker_entry.rs:1079-1125`.

Revoke-all authenticates cookie plus Origin, without freshness; revokes native
REST credentials and all account sessions via separate sweeps; does not delete
passkeys or consume outstanding auth challenges. Successful local sign-out and
qualifying 401s invalidate library/display identity. These are regression
boundaries, not permission to change session semantics. Sources:
`workers/api/src/handlers.rs:2441-2483`,
`apps/web/src/routes/account/security/+page.svelte:18-102`.

Security UI has sign-out and sign-out-everywhere only. **It is not SSR:** its
load explicitly sets ssr=false and checks session via GET vault; 401 clears
cached identity and redirects, other failures expose a warning. Do not claim
server-rendered authenticated inventory or add private data to HTML. Sources:
`apps/web/src/routes/account/security/+page.svelte:105-135`,
`apps/web/src/routes/account/security/+page.ts:8-26`.

## 3. Proposed management contract — not implemented

### 3.1 Authentication and proof

Every proposed route uses the existing authenticated, nonrevoked cookie;
mutations additionally require exact request Origin, allowed RP/origin and a
fresh session at commit. Never accept a caller-supplied email/user ID to choose
an account. All responses are private/no-store; no CDN, service-worker,
localStorage, IndexedDB or cross-account inventory cache. Read failure never
falls back to another principal's data or a guessed empty list.

**Additional management-bound proof is required, not merely a fresh cookie.**
An existing passkey assertion authorizes exactly one intended add or remove.
A proposed server-side intent binds user, current session ID, action, target
(for remove), inventory revision, RP/origin, existing proof credential, random
challenge and expiry (at most 300 seconds). It cannot be used for login,
signup, another session, a different target, or the opposite action. Required
UV/UP and all existing verifier checks remain. New credential creation never
counts as proof authorizing itself. Remove must be proven with a different,
surviving credential. A session minted through a magic link alone cannot add or
remove passkeys under this proposal.

These intent bindings/receipt do not exist in the current reauth API; ordinary
reauth must not be portrayed as already providing them. Do not change existing
session management to approximate this contract. CISO must either approve the
new management-scoped proof design, or record NO-GO; CEO must separately approve
scope and recovery UX. Existing reauth can refresh an old session before the
management proof, but cannot replace that proof. Revocation/expiry of the bound
session invalidates every uncommitted intent. A new sign-in by another account
cannot resume it.

### 3.2 Proposed shapes

Opaque management IDs are new random row references, never raw WebAuthn
credential IDs. No public key, signature, email, credential ID or session/proof
token appears in inventory, telemetry or confirmation text. Creation options
necessarily contain the owner's WebAuthn identifiers within the authenticated
ceremony; they must not be logged or persisted by the client.

| Proposed route | Request | Response and policy |
| --- | --- | --- |
| GET `/v1/passkeys` | no account selector | 200 `{revision,items:[{id,label,created_at,last_used_at:null,backup_state:"unknown"}],can_remove_last:false}`; stable creation-time then ID ordering, max eight rows; times Unix seconds, nullable where not known |
| POST `/v1/passkeys/authorize/start` | `{action:"add"\|"remove",target_id?:string,revision}` | 200 `{intent_id,challenge,rp_id,allow_credentials,expires_at}`; target required only for remove; authenticated account's eligible existing credentials only; exclude target from remove proof |
| POST `/v1/passkeys/authorize/finish` | `{intent_id,challenge,credential_id,authenticator_data,client_data_json,signature}` | 200 `{authorized:true,expires_at}`; authorization remains server-side, one action, no bearer grant returned |
| POST `/v1/passkeys/add/start` | `{intent_id,label?:string}` | 200 `{challenge,user_id,rp_id,exclude_credentials,expires_at}`; fresh, authorized add intent only; exclude this account's existing credentials; distinct management-add purpose |
| POST `/v1/passkeys/add/finish` | `{intent_id,challenge,credential_id,attestation_object,client_data_json}` | 201 `{id,revision}` after atomic insert and intent consumption; no new account/session, mail or invite; no reuse of signup finish |
| POST `/v1/passkeys/{id}/remove` | `{intent_id,revision}` | 200 `{removed:true,revision}` after atomic guard/delete/intent consumption; path ID must equal bound target; same-intent retry may replay only the same authenticated result |

Strings, lengths, enums and unknown fields are validated before any effect.
Existing 32-KiB body cap is retained. Labels: trim surrounding whitespace,
1–64 Unicode scalar values and at most 256 UTF-8 bytes; reject controls including
bidi override characters. Render labels as escaped text in bdi, never markup.
Omitted label means “Passkey”; old rows also use “Passkey” with a stable
account-scoped display ordinal, not an inferred hardware/provider name.
Label is fixed after creation in version one; rename is deferred, not silently
added to the native-REST credential surface.

Proposed admission caps: inventory 60/user/minute; authorize-start and add-start
each 10/user/hour plus shared management-start IP cap 30/hour and burst 10/minute;
finishes/remove each 100/IP/300 seconds before lookup and 10/intent/300 seconds
before proof/effect. Use IPv4/IPv6-prefix policy already present. Reserve shared
budgets atomically, bound pending intents to ten per user, prune expiry without
unbounded row growth, and deny storage failure closed. Authenticated throttle
uses registered rate_limited plus Retry-After; never silently retry mutations.
These numbers are review proposals, not observed limits. Detect historical
inventory overflow with a bounded ninth-row sentinel; never silently truncate
an account to eight rows. Overflow returns service_unavailable with incomplete
inventory copy and disables mutations pending an approved complete-list design.

### 3.3 Atomicity, removal and failure semantics

- Inventory version changes on add/remove. At commit, serialize changes per
  account; recheck active session, fresh timestamp, unexpired authorized intent,
  ownership, expected inventory version, proof-key continued existence, max-eight
  cap and last-key guard. Effect, version and proof consumption commit together.
  A failed write must roll back the effect. No check-count-then-delete race.
- Two removes observing two keys must not both leave zero. A changed inventory
  returns a generic conflict before effect, forces refetch and a new confirmation
  plus proof. No automatic rebasing/replay of user intent to another row.
- Already removed, unknown and foreign-account row IDs return indistinguishable
  not_found to the same authorized caller. Never reveal owner, email, existence
  of another account, credential count or whether an ID ever belonged to it.
  Authenticate before lookup; compare failure bodies/status and avoid
  identifier-dependent timing/disclosure. Public login/signup contracts stay
  unchanged and still reveal no inventory.
- Current version always denies final-key removal. Future enabling requires a
  server-verifiable, independently usable existing fallback sign-in method,
  recently proven by its owner and rechecked atomically at removal; client flags,
  email presence or “another passkey exists” are not substitutes. No fallback
  enrollment or new method is part of this specification.
- On uncertain commit/network timeout, read inventory before offering a new
  attempt. Do not assert success from a vanished row alone if another actor could
  have removed it; say “This passkey is no longer listed.” Same-intent result
  replay must remain session-bound and cannot authorize another effect.
- Removing a passkey prevents future assertions with that credential; it does
  not erase authenticator/sync-provider copies or end previously issued sessions.
  Do not claim otherwise or invoke revoke-all automatically. Show the existing
  “Sign out everywhere” action separately for active-session concerns.
- If revoke-all wins before a management commit, the commit fails through the
  session recheck. If mutation commits first, its completed effect remains;
  later revoke-all ends sessions. No additional session semantics are introduced.
- Secret-free audit events are proposed for management add/remove outcome and
  denied intent category, using existing internal user attribution and opaque
  management record IDs only. Never capture assertion material, email, label,
  authenticator/provider identifiers or full request bodies. Audit durability
  and transactional association must be resolved by CISO; do not promise them
  from the current best-effort audit helper.

New conflict/limit/guard codes below need registry and client-contract updates
in a separately authorized build. Do not emit unregistered strings through the
current registry (they would become internal_error). Offline `error_code` and
`next_action` are test/UX annotations, not a change to today's error envelope.

| Proposed HTTP / wire code | Offline error_code | next_action |
| --- | --- | --- |
| 401 unauthorized | unauthorized | sign_in; discard account data and intent |
| 401 reauth_required | reauth_required | refresh_session_then_new_management_proof |
| 403 forbidden | step_up_denied | cancel_or_retry_proof; no mutation |
| 404 not_found | unknown_or_foreign | refetch; reveal no account facts |
| 409 passkey_inventory_changed (new) | stale_inventory | refetch_then_reconfirm |
| 409 passkey_last_required (new) | last_passkey | add_another; final removal stays unavailable |
| 409 passkey_limit_reached (new) | inventory_limit | remove_an_unneeded_key_first |
| 409 passkey_duplicate (new) | duplicate_credential | choose_another_authenticator; generic for foreign collision too |
| 422 validation_failed | invalid_label | correct_label |
| 429 rate_limited | rate_limited | wait_retry_after; explicit retry only |
| 500 internal_error / 503 service_unavailable | storage_unavailable | retain_safe_state_then_retry_read |
| Local dismissal / unsupported / denied client storage | ceremony_cancelled / unsupported / client_storage_denied | retry_or_cancel / use_supported_browser / continue_without_persistence |

## 4. Security-page UX and copy

Keep the existing page width and typography, session actions below a separate
Passkeys section. Add no new route, sidebar, modal framework or visual brand.
Use semantic list rows with text, metadata and a named Remove button; no
horizontally scrolling management table. Never depend on color/icon alone.

Per row: escaped label, stable ordinal where labels repeat, “Added <local date>”
with machine-readable datetime, “Last used: Not available” for existing rows,
and “Sync status: Not available”. New last-used/backup metadata must not be
promised before approved storage/verifier support. If later recorded, last use
means last successful server-verified assertion, not prompt opening; backup
eligibility/state is not a provider name or proof a remote copy was erased.
If trustworthy backup flags are later supported, copy can say “Can sync” versus
“Device-bound” with an explanation and an unknown state for historic records.

| State | Visible behavior and copy |
| --- | --- |
| Initial pending | heading plus “Loading passkeys…” polite status; no stale rows/empty claim, add/remove unavailable |
| Empty, authenticated success | “No passkeys are listed.”; explain initial signup is not add; add unavailable until a supported enrollment/recovery path is approved; do not call signup or infer a lost account |
| Loaded | rows plus “Add another passkey”; optional label input only when add is chosen; no automatic ceremony on mount |
| Read failed | “Could not load passkeys. Your passkeys have not been changed.” and Retry; no mutations from stale rows |
| Add proving/creating | “Confirm it’s you to add a passkey” then “Create another passkey”; disable conflicting actions; explicit Cancel, one in-flight action |
| Add complete | refetch confirmed row, polite “Passkey added.”, focus new row heading; never replace the signed-in identity or navigate to onboarding |
| Remove confirmation | “Remove ‘<label / ordinal>’?” and “You won’t be able to use this passkey to sign in to NNTune. It may remain on your device or in your passkey provider. Existing sessions stay signed in.”; Cancel primary/default focus, separate destructive “Confirm remove” |
| Remove proof | “Use a different passkey to confirm removal.”; cancel means no finish/removal call; never fall back to a login ceremony |
| Final key | Remove unavailable with adjacent explanation: “Add another passkey before removing this one. Removing your last passkey requires a verified fallback sign-in method, which is not available here.” |
| Removed | only after confirmed result, “Passkey removed from NNTune.” plus separate active-session/provider-residue explanation |
| Conflict/unknown row | “Your passkeys changed. Review the updated list before trying again.”; refetch, no automatic destructive retry |
| Dismissal/timeout | “Passkey confirmation didn’t complete. Nothing was changed by this attempt. Try again or cancel.”; distinguish local cancellation from server rejection; if finish was sent and outcome uncertain, use reconciliation copy instead |
| Storage/network failure | “Could not confirm the change. Reload the list before trying again.”; never claim rollback when outcome is unknown |
| Session/account boundary | drop private rows/intents immediately, preserve existing auth-transition cache clearing; no previous-account label or delayed success in the new view |

One disclosure at section level: “Removing a passkey here does not remove it
from your device or passkey provider.” Optional labels are not trustworthy
hardware identities. No sensitive label is stored outside the authenticated
in-memory view; denied browser storage must not break the page.

### Keyboard, focus, screen reader, light/dark and bounds

- All entry/action controls are native keyboard-operable elements with unique
  accessible names containing label plus ordinal. Order is Add, then row actions,
  then existing session controls. Disabled final-key action has an associated
  visible reason; do not rely on a tooltip attached to an unfocusable control.
- Confirmation is an accessible labelled dialog with text description, focus
  containment, initial Cancel focus, Escape cancel and restoration to the exact
  triggering row. No focus may enter background controls while open.
- After confirmed removal, focus next surviving row heading (temporary tabindex
  -1), else previous row, else Passkeys heading. After cancel/failure restore
  the trigger when still present; otherwise use the same fallback. Announce a
  result once in the current section, not after abandonment or navigation.
- Pending uses aria-busy plus one polite status; errors use one associated
  alert, not duplicated global/section messages. Row counts and dates have
  sensible spoken text. Scope announcement assertions to this section because
  Svelte's route announcer is unrelated.
- At 320/375 CSS-pixel widths and 200% zoom, labels, metadata, confirmation and
  buttons wrap without page overflow or hidden actions; support Unicode/RTL
  labels with bdi. Existing light/dark tokens, at least 4.5:1 normal-text
  contrast, 3:1 focus/control boundaries, visible focus and at least 24-by-24
  CSS-pixel targets or compliant spacing. Test both themes and reduced motion.
- Inventory cap is eight rows. Proposed offline budget: render/update eight rows
  within 200 ms on the named CI browser after response fulfillment; request
  timeout 10 seconds with explicit Retry, not an indefinite spinner. Record
  environment/timing; this is a proposed threshold, not a measured result.

## 5. Threat model and disposition requirements

| Threat | Required mitigation / evidence before build approval |
| --- | --- |
| Stolen cookie, remove-then-add takeover | active/fresh session plus existing-key management-bound proof for each action; new key never self-authorizes; remove proof uses a surviving key; test action/session/target substitution and chained attempts |
| Shared-device or synced credential residue | no claim of provider erasure/session revocation; no inventory persistence; explicit residue copy and separate sign-out-everywhere action |
| Login/signup/reauth ceremony confusion | dedicated management purposes, RP/origin checks and intent scope; reject login or registration proof in management, and vice versa; no email/account selector |
| Cross-account enumeration/residue | authenticate before lookup, opaque management IDs, no public lists, generic foreign/missing/duplicate outcomes, no response/log metadata leaks, late-result account fence |
| Concurrent remove/add, replay, stale confirmation | per-account serialization, expected revision and atomic guard/effect/consumption; session/proof-key recheck; single-use outcome bound to session |
| Last-key lockout / assumed fallback | final-key removal NO-GO without authoritative existing fallback proof and independent usability; no email-presence heuristic or client override |
| Denied storage, partial commit/audit, network uncertainty | fail closed, atomic store contract, no client-persistence dependency, reconcile before retry, no invented success or rollback |
| Label injection, privacy and authenticator tracking | bounded escaped labels, bidi isolation/control rejection, unknown metadata, no AAGUID/provider fingerprinting or ceremony telemetry |

**CISO disposition required before implementation:** accept/reject this exact
revision's management-only proof (not existing reauth by implication), RP/origin,
method/freshness and session bindings, account/no-oracle checks, rate/cap limits,
atomicity and replay contract, last-key fallback policy, metadata/privacy,
audit/retention and session-revocation races. Require a source-bound proof/test
plan and explicitly list unresolved terms; silence or spec admission is not
approval. Any host/schema/privilege boundary remains with its authorized owner.

**CEO design disposition required before implementation:** accept/reject the
same revision's minimal UX and labels/no-rename choice, metadata unknown copy,
last-key NO-GO/recovery limitation, residue and existing-session copy, accessibility
contract and slice order. Must explicitly authorize any later build scope;
completion of this design card cannot create build cards or waive CISO terms.

**Host alignment required before build execution:** authorized executor confirms
staging RP/origin, authenticated same-origin routing, storage/migration fit and
rollback/test isolation for the approved contract. No production tests,
migrations, credential operations, new spend or new authority are authorized.
If any disposition conflicts or atomic proof is not achievable in bounded
slices, record NO-GO rather than weaken a gate.

## 6. Offline acceptance contract

All PASS-* cases below are must-pass **proposed checks, not execution results**.
Each browser test must intercept every product API route and install synthetic
credential doubles before navigation; unexpected external/product traffic fails
the test. No actual navigator ceremony, account, inbox token or stored passkey.
Use synthetic actor A/B, opaque IDs pk_a/pk_b and fixed Unix times only.

Compact future fixture set (embedded here to avoid a second source of truth):

```json
[
  {"fixture":"two","actor":"A","revision":7,"rows":["pk_a","pk_b"],"proof_key":"pk_b","target":"pk_a","expect_count":1,"error_code":null,"next_action":"announce_removed"},
  {"fixture":"last","actor":"A","revision":7,"rows":["pk_a"],"target":"pk_a","expect_count":1,"error_code":"last_passkey","next_action":"add_another"},
  {"fixture":"foreign","actor":"B","revision":7,"rows":["pk_b"],"target":"pk_a","expect_count":1,"error_code":"unknown_or_foreign","next_action":"refetch"},
  {"fixture":"stale","actor":"A","revision":6,"current_revision":7,"rows":["pk_a","pk_b"],"target":"pk_a","expect_count":2,"error_code":"stale_inventory","next_action":"refetch_then_reconfirm"},
  {"fixture":"storage","actor":"A","revision":7,"rows":["pk_a","pk_b"],"target":"pk_a","expect_count":2,"error_code":"storage_unavailable","next_action":"retain_safe_state_then_retry_read"}
]
```

| ID | Given / when / then observable acceptance | Proposed evidence |
| --- | --- | --- |
| PASS-01 | Given two owned rows, when inventory resolves, then only those rows and truthful date/unknown metadata appear. | mocked response + section snapshot |
| PASS-02 | Given zero rows, when an authenticated read succeeds, then empty copy appears without signup/add mutation. | screenshot + zero mutation count |
| PASS-03 | Given a held read, when the page loads, then loading rather than empty inventory appears. | held response + status assertion |
| PASS-04 | Given a failed read, when Retry succeeds, then usable rows replace the error with no mutation. | request count + UI snapshot |
| PASS-05 | Given two rows and eligible existing-key proof, when add completes, then one new row appears without account/session creation. | mocked traffic allow-list + row count |
| PASS-06 | Given an existing credential ID collision (owned or foreign), when add finishes, then identical duplicate copy appears without another row. | paired response/UI comparison |
| PASS-07 | Given a row, when remove confirmation is cancelled, then no proof/finish/remove request occurs. | keyboard trace + zero request count |
| PASS-08 | Given two rows and distinct surviving-key proof, when confirmed remove resolves, then exactly the named row disappears. | two fixture rows to one + named assertion |
| PASS-09 | Given the final key with no authoritative fallback, when removal is attempted, then no key is deleted. | UI guard plus backend fixture invariant |
| PASS-10 | Given a missing versus foreign ID, when a removal is attempted, then public outcomes are indistinguishable. | body/status equality + unchanged rows |
| PASS-11 | Given an old inventory revision, when removal is submitted, then it requires refetch and a new confirmation. | conflict response + no automatic resend |
| PASS-12 | Given two concurrent removes from two keys, when commits race, then at least one usable key remains. | deterministic two-order store test; final count >= 1 |
| PASS-13 | Given stale/expired session or wrong/denied proof, when add/remove is attempted, then no credential changes. | denial matrix + zero writes |
| PASS-14 | Given login/signup proof or another action/session/target's intent, when submitted to management, then it cannot authorize a change. | replay/substitution fixture matrix |
| PASS-15 | Given cancelled/timed-out/unsupported credential double, when the ceremony ends, then retry/cancel copy appears without a finish request. | error-name matrix + zero finish count |
| PASS-16 | Given server storage failure before commit, when add/remove is attempted, then state is unchanged and read retry is offered. | failure-injected store row count + screenshot |
| PASS-17 | Given a network break after possible commit, when the UI recovers, then it reconciles before another destructive attempt. | held/lost response + read-before-write order |
| PASS-18 | Given denied browser storage, when inventory/add/remove UI is used with mocks, then no private data is persisted or required to continue. | throwing storage doubles + traffic/DOM assertions |
| PASS-19 | Given actor A's held response, when sign-out/account B/navigation wins, then A's rows and success cannot appear in B's/current view. | auth-boundary harness + late-resolution assertions |
| PASS-20 | Given a keyboard-opened confirmation, when Escape/cancel closes it, then focus returns to its own trigger. | activeElement + tab containment trace |
| PASS-21 | Given the focused row is removed, when removal resolves, then focus follows next/previous/heading fallback. | activeElement after each list position |
| PASS-22 | Given 320/375-pixel light/dark views at 200% zoom, when long/RTL labels render, then all actions remain visible without overflow. | theme/width screenshots + bounds/contrast evidence |
| PASS-23 | Given a screen reader, when pending/error/success resolves, then the section announces each outcome once with named controls. | accessibility snapshot + recorded manual speech transcript |
| PASS-24 | Given markup, Unicode, bidi controls, empty/64/65-scalar or >256-byte labels, when submitted, then valid labels render literally and invalid labels cannot commit. | input boundary matrix + escaped DOM assertion |
| PASS-25 | Given eight rows or rate-limit denial, when add is attempted, then no ninth key is created and retry guidance respects Retry-After. | row count + fake-clock request count |
| PASS-26 | Given eight rows, when an offline response resolves, then render meets the recorded 200-ms budget; a 10-second held request exits pending. | environment + timing/fake-clock result |
| PASS-27 | Given existing signup/login/session actions, when their unchanged flows run, then management has not altered redirects, denial copy or cache invalidation. | existing auth/security regression bank results |
| PASS-28 | Given successful/denied/failed actions, when audit is inspected, then events contain no label, email, token or assertion material. | secret-free synthetic audit capture |
| PASS-29 | Given active proof/add, when revoke-all wins before commit, then the revoked session cannot complete management. | controlled race plus write count; reverse-order case too |
| PASS-30 | Given a removed row, when success copy is shown, then it does not claim provider erasure or sign-out of sessions. | exact copy snapshot + no revoke-all call |

### Proposed executable checks and existing test seams

Future tests should be separate owned files, not edits that weaken the existing
auth/security banks. Proposed files **do not exist yet**; the following commands
are a future acceptance handoff, not claims that a runnable management feature
or suite exists today. Every future file must name the relevant PASS-* IDs.

```sh
# Future deterministic store adapter: PASS-09..14,16..17,25,28..29.
python3 scripts/cargo_cache.py run -- test --manifest-path workers/api/Cargo.toml --test passkey_management_acceptance
# Future local mocked UI bank (run from apps/web): PASS-01..08,15,18..27,30.
pnpm exec playwright test tests/passkey-management.spec.ts --workers=1
```

The store bank must control transaction order/failpoints and compare before/after
rows, not merely mock a successful HTTP response. The UI bank must assert request
counts/order as well as visible copy; a mocked browser green is not cryptographic,
DB race or staging proof. Manual screen-reader/contrast checks remain separate.

Existing source review seams: start-body/client pins
`apps/web/src/lib/api/client.passkey-start-shape.test.ts:199-261`; browser-option
and serialization unit bank `apps/web/src/lib/webauthn.test.ts`; dismissal and
no-finish behavior `apps/web/tests/passkey-dismissal.spec.ts:175-242`; in-flight
bank `apps/web/tests/passkey-in-flight.spec.ts`; identity bank
`apps/web/tests/passkey-login-display-identity.spec.ts`; security confirmation and
failed-revoke bank `apps/web/tests/account-security.spec.ts:13-106`; load/cache
bank `apps/web/src/routes/account/security/page.test.ts`; auth transition bank
`apps/web/tests/security-auth-transition.spec.ts`. Backend synthetic ceremony
builders and reauth cases start at `workers/api/src/handlers.rs:5972-6103`;
additional verifier caps live in `workers/api/tests/webauthn_input_caps_acceptance.rs`
and `workers/api/tests/webauthn_cbor_hardening_acceptance.rs`.
These were inspected statically, not executed as feature evidence.

## 7. Unknowns, future slice limits and acceptance disposition

Unknown: live RP/origin and schema state; existing fallback proofability and
independent recovery; provider/authenticator interoperability; backup metadata
retention; authoritative audit durability; migration/locking fit for intents,
opaque IDs and revision; tested maximum inventory in deployed accounts; real
browser timings and accessibility outcomes. No deployment, credential ceremony,
API probe, migration, host change or production validation was performed.

No future cards are created. After both explicit dispositions and host alignment,
possible **at-most-one-day** slices are: (1) read-only owned inventory/opaque-ID
contract with tests; (2) offline inventory UI/unknown states; (3) management-proof
intent model and race tests; (4) authenticated add contract; (5) guarded remove
contract; (6) mocked accessible UI and regression evidence. Each slice must stay
disabled/nonoperational until its dependencies and security gates pass. These
are estimates, not authorizations. If any transaction/proof/backend slice cannot
be independently reviewed inside one day, it is **NO-GO pending a smaller design**,
not permission to start a multi-day implementation. Last-key removal and rename
remain separate NO-GO/deferred work, never silently bundled.

**Static-spec acceptance is distinct from product QA.** Source contracts,
proposal boundaries, 30 stable cases and missing proof are inspectable here.
Runtime cases PASS-01 through PASS-30 are NOT RUN. There is no staging PASS or
launch GO. A later QA verdict must identify the exact tested head and attach
actual per-case evidence before claiming the feature works.

## References and deliberate non-claims

- [GitHub passkey management](https://docs.github.com/en/authentication/authenticating-with-a-passkey/managing-your-passkeys)
  describes a passkey list, synced indicator and per-key deletion confirmation.
  The fetched page did not establish rename or a final-key fallback guard;
  neither is inherited as an NNTune security policy.
- [Google account guidance](https://support.google.com/accounts/answer/17137073)
  is a comparison reference only; the fetched content did not establish the
  task's asserted full list/rename/delete contract. No behavior above relies on
  that assertion. External product UX is not proof of our endpoint safety.
