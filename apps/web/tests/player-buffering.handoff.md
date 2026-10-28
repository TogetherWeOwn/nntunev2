# Mid-track buffering: independent RED handoff

Test-only branch: `test/mid-track-buffering`. Baseline:
`9b034604bad8c489ee71effa7544132fe5017dfe`. No standalone failing PR;
the builder carries these tests unchanged into one combined ready PR.
The task's branch/commit work products identify the immutable handoff SHA.

## Protected tests and consumer boundary

- `apps/web/src/lib/player/playback.buffering.acceptance.test.ts`
- `apps/web/tests/player-buffering.spec.ts`

No production source or existing suite is changed. If a protected test is
incorrect, return it to its author rather than weakening it.

Units use the **existing** `createPlayback` export with an optional dependency
`onBufferingChange?: (buffering: boolean) => void`. Emit only on entry (`true`)
and exit (`false`), never a redundant initial false. Subscribe to lifecycle
events on event-capable audio after acquiring it; preserve minimal `AudioLike`
callers without event methods. Only a prior current-element `playing` event,
not selection or promise success alone, qualifies a subsequent `waiting` as
buffering. Pause/end/error/unload/replacement reset it; old-element events and
late events without renewed playback intent cannot reopen it. Keep existing
start/failure callback contracts. No new required export, missing import,
production stub, timer, or provider interface is needed.

Browser tests exercise the real page and existing selectors. `player-status`
visibly says `Status: buffering` (case-insensitive) and provides polite status
semantics; recovery returns `Status: playing`. The selected item must not claim
`(playing)` during buffering. The native media boundary alone is synthetic:
media requests stay pending, `play()` is test-settled, and lifecycle events are
dispatched explicitly. External-origin requests are aborted. Mutations of the
status region count entry/recovery announcements; repeated events must not
rewrite the live text. Timer/fetch/XHR counters cover event handling and its
microtask flush, excluding Playwright's own locator polling. The initial start
calibrates the existing framework event-cleanup timeout callsite; every other
timeout site and all intervals are counted. Units forbid all controller timers
without any framework allowance. Media play/load/pause/seek counts, source,
queue order and all browser requests are also checked.

## Stable cases

| Cases | Break caught |
| --- | --- |
| BUF-U01–U04; BUF-B01–B04 | Playing/waiting/recovery, initial loading, duplicate announcements, stalled-only false diagnosis |
| BUF-U05-pause/ended/error, U06–U08; BUF-B06–B11 | Reset/fencing after native or keyboard pause, end, error, retry, track replacement, queue clear; detached events |
| BUF-U09; BUF-B05 | Hidden retry/play/load/pause/seek, new timers/network requests, source or queue mutation |
| BUF-U10–U11 | Minimal non-event callers and existing play-rejection recovery remain intact |
| BUF-B06 | Reduced-motion feedback, retained keyboard focus, labeled enabled controls, keyboard pause |
| BUF-B11–B13 | Normal end progression, end-of-track/duration sleep, no premature radio, real final-end radio continuation |
| BUF-B14–B15 | Passing negative controls: stalled remains playing; pending initial waits remain loading |

Existing queue/shortcut/comfort/radio suites stay unchanged, including the
non-repeating radio mix. No new repeat mode or transport behavior is introduced.

## Reproduce from the repository root

```sh
pnpm --dir apps/web install --frozen-lockfile
pnpm --dir apps/web exec vitest run src/lib/player/playback.buffering.acceptance.test.ts
CI=1 pnpm --dir apps/web exec playwright test tests/player-buffering.spec.ts --workers=1
pnpm --dir apps/web check
```

Do **not** use `pnpm test -- <path>` for focus: this Vitest version treats that
separator as the end of its filter options and runs the entire suite. Use
`exec vitest run <path>` as above. Browser config builds the tested checkout.
`CI=1` refuses an unrelated server on 4173. If occupied, use a run-owned config
with a free port, matching baseURL, and `reuseExistingServer: false` instead of
reusing or stopping someone else's server. On hosts with separately unpacked
Chromium libraries, supply their directory in `LD_LIBRARY_PATH`.

Observed on the baseline, 2026-10-23:

- New units: **11 assertion failures, 2 passes** (13 total). Callback history is
  `[]` instead of `[true]` or the specified longer transition sequence.
- New browser cases: **13 assertion failures, 2 passes** (15 total). All failures
  receive `Status: playing` where `/^Status: buffering$/i` is expected; the
  initial-loading and stalled-only controls pass. No run/import/build errors.
- Full web units: **1657 passes, 11 expected failures**, zero skips. All 1655
  pre-existing tests pass; only the new buffering assertions fail.
- Unchanged browser regressions below: **67/67 pass**, zero skips.
- `pnpm --dir apps/web check`: **0 errors, 0 warnings**. Baseline production
  build and standalone strict typecheck of the new browser spec also pass.

Browser evidence used this checkout's built artifact on a fresh run-owned port,
with server reuse disabled. The expected failing behavior is not fabricated by
a dummy implementation or a missing import. Post-buffering reset/recovery checks
remain unverified until the builder supplies the missing entry transition.

Regression commands (unchanged files):

```sh
pnpm --dir apps/web test
CI=1 pnpm --dir apps/web exec playwright test tests/player-playback-failure.spec.ts tests/player-lifecycle.spec.ts tests/player-queue.spec.ts tests/player-comforts.spec.ts tests/player-shortcuts.spec.ts tests/player-shortcuts-builder.spec.ts tests/radio-continue.spec.ts --workers=1
```

The builder implements the production behavior, reruns new acceptance plus
these regressions/check, then owns the combined PR through the existing gates.
No production/live-provider calls, real network-stall experiments, deployments,
new secrets, or security/approval changes are part of this handoff.
