# Mid-track buffering: implementation verification

The independent contract and stable case IDs are in
[player-buffering.handoff.md](./player-buffering.handoff.md). Original test-only
ref: `test/mid-track-buffering` at
`0b946d68a58104f05e0cba3cf69606c8e88cd371`. Test Author correction:
`test/mid-track-buffering-buf-b10-fix` at
`474533f7c4469ca00e01621f6f5bdb9b32b89b47`. Both commits were cherry-picked;
the builder did not edit either protected test file.

## Behavior and UI contract

The controller reports only buffering transitions. A prior current-element
`playing` event plus active playback intent qualifies `waiting`; play-promise
success and `stalled` alone do not. Native controls are observed without calling
`play()`. Pause, end, error, unload and source replacement reset history. The
page overlays buffering on its existing transport intent, so keyboard pause,
sleep and radio behavior stay intact. No retries, seeks, timers or requests are
introduced.

For listeners using the synthetic demo, the existing polite status region
shows `Status: buffering` on interruption and `Status: playing` on recovery.
Queue/current-track labels do not claim playing while buffering. Native and
keyboard controls remain available with the existing focus ring; no animation,
new tokens, CSS, typography, layout, dependencies or provider diagnosis is added.
The existing demo scaffold is the visual reference, not a new design direction.

## Reproduce

```sh
pnpm --dir apps/web install --frozen-lockfile
pnpm --dir apps/web exec vitest run src/lib/player/playback.buffering.acceptance.test.ts src/lib/player/playback.buffering.native.test.ts
pnpm --dir apps/web test
pnpm --dir apps/web check
CI=1 pnpm --dir apps/web exec playwright test tests/player-buffering.spec.ts tests/player-playback-failure.spec.ts tests/player-lifecycle.spec.ts tests/player-queue.spec.ts tests/player-comforts.spec.ts tests/player-shortcuts.spec.ts tests/player-shortcuts-builder.spec.ts tests/radio-continue.spec.ts tests/leveling.spec.ts --workers=1
python3 .github/scripts/check_web_bundle_budget.py
```

The bundle check requires a fresh `pnpm --dir apps/web build` (also performed by
the browser config). Use the isolated-port recipe in the handoff if the configured
preview port is occupied. Never reuse an unrelated preview or use production/live
providers.

## Observed, 2026-10-23

- Original protected units reproduced **11 assertion failures / 2 passes** before
  implementation, not import or compilation failures. After implementation:
  **13/13 pass**.
- Added native lifecycle cases reproduced two assertion failures (end-handler
  ordering cancels the next attempt; an old cleanup resets a new subscription).
  Both now pass, as does native-only playback: **3/3 pass**.
- Complete web units: **1671/1671 pass**, zero skips. Web check: **0 errors,
  0 warnings**.
- Mocked-browser buffering and unchanged failure/queue/shortcut/comfort/radio
  regressions: **82/82 pass**, zero skips. Additional leveling: **4/4 pass**.
- The first implementation run exposed an erroneous protected `BUF-B10`
  expectation: adding the first track to an empty queue already starts loading,
  rather than staying selected. Its author corrected the two expectations,
  preserving the no-buffering-before-playing and detached-event negatives.
  Production transport behavior and all existing regression files are unchanged.
- A separate local mocked-browser render check passes **2/2** for native-only
  selected → loading → playing → buffering → recovery → paused → error → empty,
  with retained focus and reduced motion at 1280×800 and 390×844. All 16 images
  received independent visual review: no material finding. The dense existing
  demo scaffold was deliberately not redesigned. Screenshots/logs are task
  artifacts, not regenerated screenshot baselines. No baseline screenshots were
  captured before implementation; original RED assertions are retained instead.
- Client JS budget: **229629 B / 230400 B**, 36 files, **771 B headroom**.

This is local synthetic demo evidence, not a backend-streaming, staging-deployment
or production claim. Exact-head CI and review are separate release gates. No
production/live-provider calls, real stall experiments, deployments, new secrets,
security changes, CLI/API, or data migrations are included.

## Beyond-the-diff check

The only production `createPlayback` caller is this demo page, which wires the
new optional callback and element-scoped observation cleanup. Existing minimal
`AudioLike` callers and callback contracts remain covered by unchanged units.
Media Session callbacks and sleep/radio transport operations need no new API;
all relevant browser regressions pass. Name-only context matches in unrelated
paywall or backend modules are not callers of these private lifecycle functions.
