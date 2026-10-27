# Device sound preference acceptance handoff

## Ownership and wire contract

This is a **tests-only assertion-RED handoff**, not a separately mergeable PR.
The Builder continues `test/device-sound-preference`, adds production code, proves
GREEN, then owns the combined ready PR and normal CI/review/merge gates. Do not
edit the four files in `apps/web/tests/sound-preference/`; return a demonstrably
incorrect assertion to the Test Author instead. Existing tests are unchanged.

Base: `9b034604bad8c489ee71effa7544132fe5017dfe` (equal to fetched `origin/main`
when work started). Resolve the handoff SHA with
`git rev-parse origin/test/device-sound-preference`; the acceptance card/work
product pins the immutable full SHA. Continue that SHA, not another agent's
unrelated worktree or an implementation-only branch. Do not repoint an active
worktree.

The test-selected public boundary is localStorage key
`nntune:sound-preference:v1` with JSON `{ "version": 1, "volume": 0.35,
"muted": true }`. The serialized object has **only** these three fields;
`volume` is a finite number in the closed interval 0..1, `muted` is a boolean,
and an absent/invalid record defaults atomically to 1/unmuted. There is no
track, account or provider identity in this device-local record. No production
function name, helper layout or new test hook is prescribed.

Accessible controls: a range named `Volume` (a descriptive suffix is allowed),
min=0, max=100, and a button named `Mute` or `Unmute` (suffix allowed), with
`aria-pressed` equal to the actual mute state. Native audio controls remain.

## Execute from repository root

```sh
pnpm --dir apps/web install --offline --frozen-lockfile
pnpm --dir apps/web exec playwright test \
  --config tests/sound-preference/playwright.config.ts \
  sound-preference.spec.ts --reporter=json > sound-preference-results.json
```

Exit 1 is expected on the handoff base; GREEN must exit 0 without skipping any
new case. JSON includes each stable `SP-*` case, assertion, result and totals.
Keep generated results/traces outside tracked source (prefer the run scratch
directory). The runner builds and owns a dedicated localhost preview on 43817,
uses one Chromium worker and zero retries, refuses an occupied port, and stops
its own server. `SOUND_ACCEPTANCE_PORT` can select another free local port. Never
reuse an arbitrary process on the repo's usual shared preview port.

Chromium must already be installed. If this sandbox's cached browser needs its
provided libraries, verify the directory exists and prefix commands with
`LD_LIBRARY_PATH=/paperclip/.cache/chrome-deps/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}`.
No download, new service or backend is needed. After the locked dependency
install, fixture execution is offline: tests generate and serve an original
16-second silent PCM WAV, including truthful byte-range responses for native
seeking, at the demo audio route. Except in the two explicitly
named setter simulations, real browser volume/mute accessors, native events,
play/pause, seeking and AudioContext remain real. `play()` is observed and then
delegated, never replaced by a fake promise. A preexisting mount-only leveling
probe provides the hydration barrier and the ReplayGain observation; no
production stub/hook was added. Seek-preservation cases set `currentTime`
after real metadata loads, isolating volume behavior from pre-metadata URL
seek timing; the existing comfort suite retains its separate share-link tests.

Regression commands (same isolated preview configuration):

```sh
pnpm --dir apps/web exec playwright test \
  --config tests/sound-preference/playwright.config.ts \
  player-shortcuts.spec.ts player-shortcuts-builder.spec.ts \
  player-comforts.spec.ts leveling.spec.ts radio-continue.spec.ts \
  --reporter=json > sound-regressions.json
pnpm --dir apps/web test
pnpm --dir apps/web check
```

## Stable case map / mutation intent

| Cases | Break caught |
| --- | --- |
| `SP-PARSE-01..05` | Ignoring saved state, truthy-zero coercion, restoring after play instead of before it |
| `SP-INVALID-01..25` | Unknown version, malformed JSON, invalid types, missing fields, nonfinite parsed numbers (`1e999`), negative/above-one values; mount and real transport must survive |
| `SP-UI-01`, `SP-CUSTOM-RELOAD` | Missing accessible controls, implicit unmute/play, losing chosen level or mute on reload |
| `SP-NATIVE-00/35` | Native events not absorbed/serialized; data outside the small schema; state lost after reopening/reload |
| `SP-TRACK-00/35` | Keyed audio replacement starts at unity/unmuted rather than the device choice |
| `SP-STALE` | A detached element updates current state or persistence |
| `SP-SETUP` | Transient setup events overwrite saved values, even if a later write repairs them |
| `SP-LOOP` | Incoming native events amplify into continuing writes/events or extra play calls |
| `SP-STORAGE-getter/read/write` | Unguarded storage property/method/write exceptions break mount, transport or live state |
| `SP-INDEPENDENT` | Preference changes alter ReplayGain, rate, seek, paused/playing intent or invoke extra play |
| `SP-SETTER-throw/ignore` | Unsupported setters crash transport or the UI claims a level the element did not accept |
| `SP-A11Y` | Volume/mute cannot be reached/operated by keyboard, missing visible focus or meaningful state |

Parser/serializer vectors exercise the **real app through its persisted
boundary**, including actual `JSON.parse` overflow; they are not a duplicate
parser written in test utilities. No standalone pure sound-preference API
exists on the base, so there are no speculative missing-module imports or
unit tests against dummy production stubs. If the Builder extracts a pure
helper, add its own unit tests in addition to (not instead of) this protected
black-box matrix. Storage fakes only inject denial/failure; assertions concern
the actual player, media properties, transport and persisted values.

`SP-SETTER-*` is simulated restricted-platform behavior, **not hardware-volume
or mobile-device acceptance**. Normal Chromium cases prove element volume,
not device output loudness. Real devices that prohibit writes must remain
truthful and usable; system hardware volume is out of scope. No new CLI/API
was added; dry-run is N/A.

## Recorded handoff verification

- `pnpm --dir apps/web test`: exit 0, **107 files / 1,655 tests passed**.
- `pnpm --dir apps/web check`: exit 0, **0 errors / 0 warnings**.
- Combined browser comparison (the focused command plus all five regression
  filenames above): exit 1, with all **47 protected regression cases passed**:
  shortcuts 8, builder shortcuts 8, comforts 17, leveling 4, radio 10. The
  combined exit is RED due to the new sound tests, not a baseline failure.
- Extra standalone test-file `tsc --ignoreConfig ... --types node` attempt:
  exit 2, **TS2688: Cannot find type definition file for 'node'**. The locked
  dev dependencies do not expose standalone Node type definitions; no
  dependency/config change was made. This tooling limitation is not counted
  as assertion-RED. The application check and real Playwright execution above
  are separate results.
- Final focused browser command above: **exit 1; 18 assertion failures,
  28 passes, 0 skips, 0 flaky, 0 runner errors**. Eleven failures compare real
  media/persistence values; seven assert absent accessible controls. Examples:
  `SP-PARSE-01` expected `{ volume: 0.35, muted: true }`, received
  `{ volume: 1, muted: false }`; `SP-NATIVE-35` expected the three-field saved
  record, received `null`; `SP-TRACK-35` expected volume 0.35 on the replacement,
  received 1 (mute correctly remained true). These are assertion failures,
  not absent imports, syntax/build failures or dummy source stubs.
- The first fixture iteration answered native byte-range requests with 200;
  this broke paused seek preconditions. The final fixture answers truthful
  206 ranges instead. Both denied-storage cases now pass with real seek and
  real playback; no product code was changed for this harness issue.

GREEN and the combined PR remain the Builder's next action. No production
implementation was read to design these tests, and no production or existing
protected test file was edited. Later assertions within each RED case have not
yet executed past the first failing assertion; the Builder must run all 46
cases to completion, without skips, to establish GREEN.

Skills used: paperclip, paperclip-superpowers-adapter, test-driven-development,
verification-before-completion. Planning/execution skills are installed but not
enabled for this agent; the enabled adapter's direct-work fallback was used.
