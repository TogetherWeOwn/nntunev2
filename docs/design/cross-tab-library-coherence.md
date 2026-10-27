# Cross-tab library coherence: static specification

Status: proposed, documentation only. Acceptance is not implementation approval.
No code, storage schema, account-sharing boundary, server sync, telemetry, or
build tasks are introduced. This file is the single repository source of truth;
review packets link to its exact commit rather than maintaining a second spec.

## Decision and corrected premise

**Do not implement general cross-tab library/favorites/queue synchronization.**
The current storage area is not the same thing as the current sharing scope:

- Favorites and recent finds are in `localStorage`, but their key suffix comes
  from a random **tab-scoped session context**, not an account identifier.
  Independently opened tabs ordinarily have different contexts. An opener or
  duplicated tab may start with a copied context; that is not proof of identity.
- Library snapshots are already in `sessionStorage`. The old shared local
  snapshot is cleanup-only. A refresh in tab A is not a shared cache write that
  tab B should import.
- The actual library queue is component memory, not shared storage. Only the
  demo queue has session persistence at the examined revision. Preserve both
  scopes. Never substitute demo behavior for library behavior.

A same-origin event grants no authority to adopt another tab's data. Recommend
an exact-current-context **refresh prompt** for favorites only, if separately
accepted, and documented stale-ok behavior elsewhere. Do not discover other
contexts, copy suffixes, introduce an account key, or make session queues shared.
The existing auth-transition purge remains higher priority than every proposal.

## Examined source and inventory

Examined head: `0052c8434924b267f3c7a998312f9051e9779418`, 2026-10-27.
The following line ranges are a point-in-time inventory, not claims that an
unmerged branch or a future implementation is present. Reinspect before build.

### Source map

| Ref | Source at examined head | Relevant behavior |
| --- | --- | --- |
| S1 | `apps/web/src/lib/library/cache.ts:30-60` | Key definitions and random session-context construction |
| S2 | `apps/web/src/lib/library/cache.ts:192-281` | Snapshot I/O, purge, auth-transition write/remove |
| S3 | `apps/web/src/lib/library/favorites.ts:21-70` | Context-key resolution, full-array read/write, denied-storage behavior |
| S4 | `apps/web/src/lib/library/recent-finds.ts:30-110` | Context-key resolution and recent-query I/O |
| S5 | `apps/web/src/routes/library/+page.svelte:261-350` | Session snapshot dependency, queue reconciliation, once-per-visit hydration |
| S6 | `apps/web/src/routes/library/+page.svelte:1370-1451` | Local/delivered auth purge, subscriptions and teardown |
| S7 | `apps/web/src/lib/library/refresh.ts:240-335` | Response classification, last-good memory, generation fencing |
| S8 | `apps/web/src/lib/auth-transition.ts:4-17` | Qualifying same-window events; explicit best-effort delivery contract |
| S9 | `apps/web/src/routes/demo/player/+page.svelte:1499-1506` and `apps/web/src/routes/demo/player/+page.svelte:1594-1614` | Demo queue persistence and preference bindings |
| S10 | `apps/web/src/lib/player/queue.ts:273-305` | Demo serialization contains order/current IDs, not a shared queue |

In the tables below, `<context>` means the value of the session context key in
**this tab**, never an account ID. Bare library base keys are legacy cleanup
names, never a fallback source of readable user data.

### Library, identity and queue keys

| Exact key or state | Area; writers / readers / removal | Present staleness window | Recommended policy |
| --- | --- | --- | --- |
| `nntune.library.auth-context.v1` | Session; snapshot save creates it; snapshot/favorites/recent-find helpers read it; auth purge removes it (S1-S4) | Survives reload in that tab until purge or tab session ends; no account identity or clock semantics | Stale-ok as an opaque scope token; never live-sync, transmit, scan for, or recreate from a peer |
| `nntune.library.snapshot.v1.<context>` | Session; authenticated refresher saves, guarded fallback reads, purge removes (S2, S5, S7) | Another tab cannot replace it. Current page stays on its last successful list until its own refresh/auth reset/unmount; no age TTL | Stale-ok cross-tab; retain existing same-tab Refresh and stale badge |
| `nntune.library.snapshot.v1` and legacy local context/scoped snapshots | Session/local legacy cleanup; clear helpers and library mount remove; no current production local snapshot saves (S2, S6) | Never an admitted hydration source | Stale-ok/unreadable legacy data; do not revive or migrate |
| `nntune.library.auth-transition.v1` | Local transient signal; invalidation writes a unique opaque marker then removes it; library/provider/BYOP receive events (S2, S6, S8) | Receiver resets when a delivered event is processed. No acknowledgement or all-tabs delivery deadline | Live **invalidation**, never data sync; preserve exact current contract |
| `nntune.library.favorites.v1.<context>` | Local; bootstrap reads; star/unstar, reorder, remove and clear write full arrays; boundary removes scoped and bare keys (S3, S5; library page lines 1059, 1106, 1222, 1251) | Hydrated once after successful authenticated load. Even Refresh does not reread during that visit. Same-context peer edits can remain unseen until remount/reload; different-context edits are out of scope, not a shared-key update | Refresh-prompt only for an exact-current-context external change; no silent replacement or account-wide sync |
| `nntune.library.recent-finds.v1.<context>` | Local; once-per-visit read; query-record/reorder/clear full-array saves; auth purge removes scoped and bare keys (S4-S6; library page lines 1177, 1189, 1196) | Memory authoritative this visit; external writes do not hydrate it again on Refresh | Stale-ok; private query history is not worth cross-tab activity or chatter |
| Library queue | Component memory; queue actions write, UI/player read, refresh prunes missing IDs, auth reset clears (S5-S6; library page lines 129, 627-636) | No cross-tab or reload persistence at this revision | Stale-ok/independent; no queue changes, prompts or playback from another tab |
| `nntune:demo-queue:v1` | Session; demo reactive save and mount restore (S9-S10) | Each tab owns its arrangement; opener copies can subsequently diverge | Stale-ok/independent; session persistence stays session-scoped |
| `nntune:signed-in-email` | Session; display helper read/write/clear: `apps/web/src/lib/signed-in-email.ts:11-31` | Display label changes through existing auth flows, not another tab's label writes | Stale-ok display only; clear on delivered auth reset; never authorization or channel data |

### Other Web Storage keys in the examined web application

These are inventoried to prevent accidental prefix-wide synchronization. None is
an authorization source or part of the proposed favorites prompt.

| Exact key | Area; writer / reader evidence | Staleness and recommended policy |
| --- | --- | --- |
| `nntune:density:v1` | Local; library read/write helpers: `apps/web/src/routes/library/+page.svelte:74-92` | Read on library mount; memory until remount or local change. Stale-ok: don't move another tab's active controls |
| `nntune:appearance:v1` | Local; layout read/write: `apps/web/src/routes/+layout.svelte:18-46` | Existing layout hydration/local action, no new peer listener. Stale-ok in this scope; separate preference work could choose live-sync |
| `nntune:radio-continue:v1` | Local; demo read/write: `apps/web/src/routes/demo/player/+page.svelte:268-289` | Existing demo state until next read/local change. Stale-ok; no remote autoplay policy changes |
| `nntune:playback-speed:v1` | Local; `apps/web/src/lib/player/playbackSpeed.ts:9-61` | Consumers load/save their preference, not peer-driven playback. Stale-ok while mounted |
| `nntune:resume:v1` | Local; `apps/web/src/lib/player/resume.ts:20-110` | Snapshot read/write/clear through existing resume flow. Stale-ok; never seek or resume in response to peer activity |
| `nntune:demo-recently-played:v1` | Session; `apps/web/src/lib/player/recentlyPlayed.ts:15-125`; S9 binds session storage | Tab-local history until next local load/action. Stale-ok; do not share |
| `nntune.leveling.enabled` | Local; `apps/web/src/lib/player/replayGain.ts:20-162` | Existing read/local-save lifecycle. Stale-ok; no surprise change to another tab's audio |
| `nntune:sound-preference:v1` | Local; `apps/web/src/lib/player/soundPreference.ts:7-49` | Existing read/local-save lifecycle. Stale-ok; never change active playback from an event |

### Exact present refresh limits

A fresh successful `/api/library` response populates memory and a capped session
snapshot. Persisted limits are 200 tracks and 200 KiB; memory keeps the full
fresh list (`apps/web/src/lib/library/cache.ts:63-67`, S7). A later transient
failure may show last-good memory with `stale — updated …`; there is no TTL.
An external write alone neither refreshes a list nor gives it that badge.

A new page's failed first fetch must not expose stored rows: successful
authentication in that page is required before fallback. HTTP 401/403 invalidate
and broadcast; 401 offers sign-in and 403 access-denied. HTTP 404 empties the
visible list and in-memory trust but does not call persistent invalidation (S7).
Favorites/recent-find memory intentionally survives ordinary Refresh so a denied
write cannot erase pending visit edits (S5). Do not silently change that rule.

## Transport and protocol, if the narrow prompt is accepted later

Choose **native `storage` events only**. This is an invalidation hint protocol,
not a data exchange. There is no new persistent signal key, envelope, channel,
clock, or revision field. The auth signal name remains exactly
`nntune.library.auth-transition.v1`; favorites routing uses the exact key
returned by the existing current-context key helper. Neither bare base-key nor
prefix matches authorize reading another context.

| Capability | Support / fallback |
| --- | --- |
| Window `storage` event | MDN Baseline widely available since April 2017. `localStorage` events reach other same-origin browsing contexts, not the writer |
| Session storage | Events are restricted to the same top-level browsing context, not independent tabs. Never use this as a cross-tab transport |
| BroadcastChannel | MDN Baseline March 2022; structured clone, same-origin **and storage-partition** limits, sender channel object excluded. **Not selected**: no extra benefit for a prompt, and no new auth fallback is admitted |
| Unsupported/blocked storage or missed delivery | Existing visit memory, reload/explicit authenticated refresh behavior; no polling, hidden retries, alternative identity channel, or global-convergence claim |

### Routing and rereads

1. Install at most one listener per mounted consumer, with idempotent cleanup.
   Preserve today's auth listener/reset semantics; a generic data handler must
   not swallow, debounce away or postpone a delivered auth transition.
2. The prospective favorites listener resolves the local area through the
   existing no-throw resolver and checks `event.storageArea` identity and the
   exact current favorites key. Never read `event.oldValue`, `event.newValue`
   or `event.url` as data, authority, a fetch address, or an ordering clock.
3. An exact-key notification only marks one in-memory `updated elsewhere`
   notice. It does not parse favorites, fetch, save, merge or change membership.
   The writer continues its existing local update; the writer receives no own
   native event and must not synthesize a second save/notification.
4. On an explicit foreground action to apply the change, use the existing
   authenticated refresh path first; then reread **current** storage through
   the current-context helpers only if the same mount/auth generation and key
   still hold and no unsaved edits exist. A new implementation must distinguish
   readable absence from denied/malformed storage; existing helpers alone
   collapse these into an empty list. Until that distinction is proven without
   a persistent schema change, this apply action is **NO-GO**.
5. A key removal is still only a hint; accepting readable absence means an
   empty saved set, not the event's null payload. `clear()` has `key === null`:
   the prospective data handler may invalidate its pending prompt/read intent,
   but must not invent a login, infer a principal, or broaden today's auth
   handler. Library/BYOP currently require the exact auth key; provider also
   handles clear. Harmonizing that difference is separate auth-policy work.

### Ordering, conflicts and teardown

- No wall-clock comparisons. `savedAt` is badge provenance, not cross-tab
  order. Read the current store at apply time; queued event payloads may
  describe values already superseded. A per-mount **in-memory** generation
  fences delayed work, with a fresh generation on disposal/auth reset.
- Full-array writes are last-successful-storage-write wins, not transactions.
  Two overlapping read/modify/write operations can lose independent stars or
  a reorder. Do not promise set union, CRDT behavior, cross-key atomicity or
  a safe queue merge. A read-before-write cannot make this atomic.
- Clean receiver: one optional explicit apply can replace the whole saved
  array while retaining IDs absent from the loaded library. Dirty receiver
  (including any unconfirmed write): preserve memory and say `Changes are
  pending in this tab. Reloading may discard them.` Do not autosave or merge;
  a later deliberate full-list save may replace the stored winner.
- Receivers never write or rebroadcast merely because of an event. Coalesce
  repeated favorites hints into one notice; the existing auth signal's
  write/remove pair must not become a reload/rebroadcast loop. Equal stored
  values may produce no event; no receipt or completeness claim is inferred.
- Unmount: remove listeners, cancel pending work/notices, invalidate generations.
  Repeated cleanup is safe. A bfcache-restored view must not apply an old queued
  result: re-enter the existing authenticated load path before any proposed
  apply. No intervals or background polling are introduced.

## Auth boundary, failure behavior and threat model

**A delivered sign-out/revoke/auth-transition event must still purge every
listening consumer before any queued data apply can commit.** Clear favorites
and recent-find keys while the outgoing context is still known; then revoke
that context, abort/fence refresh work, clear queue/selection/export/share
feedback and the display email, and use the existing signed-out presentation
(S6-S8). Do not change same-window notification eligibility: qualifying security
success/typed 401 notify; sign-in completion is not a new local security event.

Do not automatically reread, fetch, navigate, focus or rebroadcast from that
receiver. A later user-driven authenticated load may hydrate a new context;
old queued keys, prompts, late fetches and exports cannot survive into it.
Never import a different context even if both tabs appear to use the same email.

Storage getter, read, set or removal may fail independently. Keep working visit
memory after ordinary persistence failures; do not falsely label a write saved.
On auth reset, clear memory even when durable deletion/broadcast fails. Denied
storage does **not** justify copying data through BroadcastChannel. Remote tabs
cannot be guaranteed to reset if no signal can be delivered: the existing
contract explicitly excludes suspended, closed and non-listening tabs and has
no acknowledgement. This spec preserves that limitation rather than asserting
an impossible global wipe. Server checks remain authoritative for access.

| Threat / ambiguity | Required containment |
| --- | --- |
| Same-origin script or forged event | Events are untrusted hints; never credentials, principal evidence or executable instructions. Existing XSS defenses remain necessary; storage is not an authorization boundary |
| Copied tab context or cookie/account switch during work | Context equality is not account equality. Never broaden scope, scan sibling keys or reuse a pre-reset result; delivered auth reset wins |
| Peer history/track disclosure | No new message payloads, identity labels, tokens, session material, queries, IDs or library rows on a channel; use existing key routing only |
| Denied storage, stale events, crash, lost writes | Preserve no-throw/dirty-memory behavior; no guaranteed convergence, atomic merge or global purge claim |
| Background activity interfering with user controls | No automatic focus, playback, scroll, selection or live-region feedback from data events |

**No new security boundary is proposed**, so no new CISO/CEO security disposition
is requested. Ordinary design acceptance is not permission to build. Account-wide
favorites, new account-bound keys, new secret/session transport, acknowledged
all-tab logout, or a changed purge policy are explicit **NO-GO** here; they would
need a revised design and appropriate CISO/CEO disposition before implementation.

## User-visible behavior and accessibility

| Surface / event | Behavior |
| --- | --- |
| Favorites, same exact context | Optional nonmodal `Favorites were updated in another tab.` notice only; no silent star/reorder change. Apply remains gated above; no notice for another context |
| Favorites with local pending edits | Preserve the entire visit arrangement and hidden membership; explain discard risk rather than promising a merge |
| Library list, real/demo queue, recent finds and audio preferences | No cross-tab updates or prompts. Existing user Refresh, reload, stale badges and local controls remain the only actions |
| Auth transition | Existing purge/sign-in-required presentation; it preempts all optional notices. No new event-driven navigation or requests |

Background data events must not call focus, scroll, play, seek or a live announcer.
When hidden, retain at most one pending notice in memory, with no `role=status`
mutation. Visibility alone does not speak or apply data. A foreground notice is
ordinary discoverable text with a keyboard-reachable action, not a dialog.
After an explicit apply, at most one polite outcome; retain action focus if the
node survives. If applying would remove the focused row/control or disrupt an
active edit, defer to an explicit reload choice instead of silently moving focus.
Existing security alerts are not suppressed by this data-notice policy.

## Offline fixtures and stable proposed cases

Design fixtures only, not a new storage schema. Two fake tab environments share
one fake local map and have independent session maps, auth generations, memory,
focus and announcement spies. No cookies, network, real browser tabs or real
multi-tab automation are needed. Cases marked candidate are **not implemented**.

```json
{
  "items0": [{"id":"track-a","title":"Alpha"},{"id":"track-b","title":"Beta"},{"id":"track-c","title":"Gamma"}],
  "items1": [{"id":"track-b","title":"Beta"},{"id":"track-c","title":"Gamma"}],
  "tabA": {"context":"ctx-a","generation":1,"favorites":["track-a","missing-id"],"queue":["track-a","track-b"]},
  "tabB": {"context":"ctx-b","generation":1,"favorites":["track-c"],"queue":["track-c","track-b"]},
  "copiedContext": "ctx-a",
  "savedFavorites0": ["track-a","missing-id"],
  "savedFavorites1": ["track-b","missing-id"],
  "savedFavoritesReordered": ["missing-id","track-b"],
  "savedAt": "2026-10-27T00:00:00.000Z"
}
```

The fixture adapter derives existing scoped keys from context, rather than
persisting this envelope. Inject read/write/remove/getter failures separately.
A copied-context variant deliberately replaces B's context with `ctx-a`; it is
not the default and must never be called account equivalence.

| Case ID | Offline stimulus | Required oracle / status |
| --- | --- | --- |
| XTAB-01 | Independent A/B; A stars then unstars `track-b` | B's different-context favorites, rows and queue do not change; no prompt |
| XTAB-02 | Copied context; A stores favorites1 then its reorder; deliver both events after final storage write | Candidate: one prompt, no silent change; explicit clean apply reads final reordered storage, never older event values |
| XTAB-03 | Copied context; A and B make conflicting full-array writes from favorites0 | Last successful stored array wins; dirty visit state is retained, hidden `missing-id` is not silently lost; no union/atomicity claim |
| XTAB-04 | Reorder, select, clear or advance queue in A | B real/demo queue, current item, playback and focus stay unchanged; no session queue transport |
| XTAB-05 | A refreshes to items1 while B retains items0 | Only A session snapshot/list and own queue reconciliation change; B does not import A's snapshot |
| XTAB-06 | Deliver exact-key removal, clear/null key, bare legacy key and unrelated/sibling-context keys | Candidate: removal is a hint; readable absence distinct from failure; null key never invents auth or imports a peer; unrelated keys ignored |
| XTAB-07 | Getter/read/set/remove denial, quota refusal and malformed favorites JSON | No throw, no fake saved success, no auto-overwrite with empty parse fallback; keep pending memory; no all-tab guarantee |
| XTAB-08 | Queue data apply/late fetch under generation 1, then deliver auth transition and rotate to generation 2 | Purge before any apply; old contexts, rows, queue, email, queries and export/share outcomes cannot return; no rebroadcast/request/navigation |
| XTAB-09 | New tab, no context; first fetch fails; then successful load | No initial cached disclosure; fresh load creates its own scope. Copied-context variant still requires authenticated page load before hydration |
| XTAB-10 | Hidden B receives repeated data hints, becomes visible, then user activates a notice | Candidate: one notice, zero background focus/scroll/play/live announcements; at most one user-initiated polite outcome |
| XTAB-11 | Deliver duplicate auth set/remove events, dispose twice, then deliver late data work | Existing reset remains idempotent/no-echo; disposed consumer cannot mutate, announce or save |
| XTAB-12 | Storage signal cannot be written/delivered; local qualifying security action completes | Same-window subscribers still reset memory; remote convergence remains explicitly unproven until existing checks run |

### Executable proposed checks, not newly authored acceptance tests

From `apps/web`, existing baseline checks can be run without a server:

```sh
pnpm exec vitest run src/lib/library/favorites-arrangement-storage.test.ts --maxWorkers=1
```

A future Test Author may add a **new owned** offline unit bank and select cases
by the XTAB IDs; do not edit existing acceptance tests to force this design.
The fake-harness contract above must prove all event orders for XTAB-03/08,
including event delivery after disposal and a denied read distinct from absence.
Until that bank exists, the XTAB rows are specifications, not passing tests.
Existing browser acceptance banks are evidence of constraints, not a request to
add real multi-tab tests:

- `apps/web/tests/library-cache.spec.ts:278-430` — existing cross-tab auth and
  storage-getter-denied behavior.
- `apps/web/tests/security-auth-transition.spec.ts:634-658` — no-throw local
  reset under getter/write/removal denial.
- `apps/web/tests/library-favorites-management.spec.ts:394-449` and
  `apps/web/tests/library-favorites-arrangement.spec.ts:231-259` — pending
  memory, hidden membership and recovered full-list-save contracts.

Static validation from the repository root:

```sh
python3 .github/scripts/docs_link_lint.py --files docs/design/cross-tab-library-coherence.md
python3 .github/scripts/docs_citation_lint.py --files docs/design/cross-tab-library-coherence.md
```

## Future slices and explicit stop conditions

These are estimates for a later, separately authorized build, **not created
cards**. No slice is authorized by this document's acceptance.

| Future slice | Bound / evidence required before proceeding |
| --- | --- |
| Offline fake-tab contract bank | At most 1 day; XTAB-01..12, exact-key routing, dirty-state and auth-fence oracles; no real multi-tab automation |
| Same-context favorites notice only | At most 0.5 day after accepted spec and tests; one listener/notice, no hydration, network or write side effect |
| Explicit clean-state apply | At most 1 day only after readable-absence vs denial/corruption and unsaved-write detection are proven without changing persistence; otherwise NO-GO, keep notice/reload explanation only |
| Account-wide favorites/history sharing; live queue/cache sync; new storage schema or auth transport | NO-GO in this scope. Separate identity/provenance and conflict design plus applicable dispositions are prerequisites |

Rollback for any later narrow UI slice: remove the listener/notice/apply wiring;
existing keys, local actions, tab-local queue and auth reset remain unchanged.
No data conversion or deletion is part of this proposal.

## Browser references

- [Window storage event](https://developer.mozilla.org/en-US/docs/Web/API/Window/storage_event)
  — sender exclusion and local/session recipient scopes.
- [StorageEvent properties](https://developer.mozilla.org/en-US/docs/Web/API/StorageEvent)
  — clear/null key, removed/null value and storage-area identity.
- [Broadcast Channel API](https://developer.mozilla.org/en-US/docs/Web/API/Broadcast_Channel_API)
  — support baseline, partition restrictions, structured clone and close teardown.
