# Mid-track buffering: implementation verification

The independent contract and stable case IDs are in
[player-buffering.handoff.md](./player-buffering.handoff.md). Original test-only
ref: `test/mid-track-buffering` at
`0b946d68a58104f05e0cba3cf69606c8e88cd371`.

The controller reports only buffering transitions. A prior current-element
`playing` event plus active playback intent qualifies `waiting`; play-promise
success and `stalled` alone do not. Native controls are observed without calling
`play()`. Pause, end, error, unload and source replacement reset history. The
page overlays buffering on its existing transport intent, so keyboard pause,
sleep and radio behavior stay intact. No retries, seeks, timers or requests are
introduced. The same polite status region announces entry and recovery; controls
and focus are not replaced.

## Reproduce

```sh
pnpm --dir apps/web install --frozen-lockfile
pnpm --dir apps/web exec vitest run src/lib/player/playback.buffering.acceptance.test.ts src/lib/player/playback.buffering.native.test.ts
pnpm --dir apps/web test
pnpm --dir apps/web check
CI=1 pnpm --dir apps/web exec playwright test tests/player-buffering.spec.ts tests/player-playback-failure.spec.ts tests/player-lifecycle.spec.ts tests/player-queue.spec.ts tests/player-comforts.spec.ts tests/player-shortcuts.spec.ts tests/player-shortcuts-builder.spec.ts tests/radio-continue.spec.ts --workers=1
```

Use the isolated-port recipe in the handoff if the configured preview port is
occupied. Never reuse an unrelated preview or use production/live providers.

## Observed before the protected-test correction, 2026-10-23

- Original protected units reproduced **11 assertion failures / 2 passes** before
  implementation, not import or compilation failures. After implementation:
  **13/13 pass**.
- Added native lifecycle cases reproduced two assertion failures (end-handler
  ordering cancels the next attempt; an old cleanup resets a new subscription).
  Both now pass, as does the native-only playback case: **3/3 pass**.
- Complete web units: **1671/1671 pass**, zero skips. Web check: **0 errors,
  0 warnings**.
- Mocked-browser buffering and unchanged failure/queue/shortcut/comfort/radio
  regressions: **81/82 pass**. All **67 existing regressions pass**; buffering
  cases pass **14/15**. The remaining assertion in `BUF-B10` expects `selected`
  after adding a first track to an empty queue. Existing `addToQueue` starts
  playback, so the pending media mock correctly shows `loading`. The protected
  test has been returned to its author; it has not been weakened or bypassed.
- A separate local mocked-browser render check passes **2/2** for native-only
  selected → loading → playing → buffering → recovery → paused → error → empty,
  with retained focus and reduced motion at 1280×800 and 390×844. Screenshots and
  logs are attached to the implementation task, not committed as baselines.

**Not a final PASS:** the protected-test correction, final exact-head browser
run, CI and review are still required. This is local synthetic demo evidence,
not a backend-streaming, staging-deployment or production claim.
