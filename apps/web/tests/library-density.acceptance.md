# Track-list density acceptance handoff

Test-only, deliberately RED. Base: `a242bbc721f17e736757dadc4a6f85ba396e2d3f`.
Handoff ref: `test/library-density-acceptance`. The builder continues this
exact branch in the **one combined implementation PR**; do not open a
separate knowingly-red test PR. No production source/config was changed.

## Protected files and consumer contract

Preserve these files; implement the app, not replacement test behavior:

- `src/routes/library/density.acceptance.test.ts` — real library-page SSR
  (established refresher-snapshot pattern; existing default export).
- `tests/library-density.acceptance.spec.ts` — browser geometry, keyboard,
  persistence, storage-refusal and cross-view cases.

Paths are relative to `apps/web`. No new production function export or
test-only production seam is required. Smallest UI contract, mirroring the
appearance preference: one visible native select labeled **Density** on the
library page, options **Comfortable**, **Compact** with values `comfortable`,
`compact`, in that order. Comfortable is the SSR/default choice; the server
render must not read browser globals or device storage. Store only the raw
validated enum under `nntune:density:v1` in localStorage — never the
appearance key `nntune:appearance:v1`, no identity, cookie, sessionStorage
or HTTP write. Unknown, padded, mixed-case, JSON, blank and missing values
mean Comfortable. Storage read/write refusal for that key must preserve a
usable page and the in-memory choice, with no error banner.

Compact must measurably reduce rendered library-row and queue-row height
(padding/typography steps only; the bank compares the same real rows before
and after, requiring at least 1 CSS px less) without hiding title/artist
text, changing Tab order, or weakening visible focus outlines. No root
attribute, class, module name or mechanism is prescribed. The choice
applies to the browse list and the queue list on the library page across
find, ordering and favorites use, and survives reload on the same device.
Whether favorite-section rows themselves compact is deliberately **not**
pinned; their text visibility and target sizes are guarded either way.

Target size follows WCAG 2.5.8 AA as written: 24×24 CSS px **or** the
spacing exception (a 24px-diameter circle centered on an undersized target
intersects no other target/circle), plus a stricter density rule: Compact
must not shrink any target below its Comfortable size. Measured baseline
reason: existing queue-row action buttons (Move/Play next/Remove/Select
text buttons) render **21px tall** at the base above and pass 2.5.8 only
via spacing, so pinning an absolute 24×24 would make the bank fail for a
pre-existing condition instead of the absent feature. Raising those buttons
to a true 24px is compatible with every case here if the build chooses to.

## Stable case families

| IDs | Observable protection |
| --- | --- |
| DEN-SSR-01..02 | Two choices/Comfortable default with the Density label in server markup; no browser-global or storage read during SSR (trapped) |
| DEN-SSR-03 (control, passes at base) | Existing rows keep title/artist text in server markup |
| DEN-00 (control, passes at base) | Real mocked library, queue, find/order/appearance controls, WCAG target sizes already work; no HTTP writes |
| DEN-01 | Control shape: accessible name, option text/values/order, Comfortable start, ≥24px select box |
| DEN-02 | Compact shrinks **both** lists' rows; title/artist stays visibly rendered (text-range geometry, not textContent); Comfortable restores geometry |
| DEN-03 | WCAG 2.5.8 holds in both densities; Compact adds/removes/shrinks no target |
| DEN-04 | Identical captured Tab sequence in both densities; select reachable and operable by keyboard (Home/ArrowDown/Enter); visible focus outline kept |
| DEN-05 | Only the raw enum is written (audited `setItem`); both choices survive reload; appearance key untouched; no HTTP writes |
| DEN-06-{unknown,case,padded,json-string,json-object,empty} | Invalid stored values fall back to Comfortable |
| DEN-07-{read,write} | Key-scoped storage refusal: default applies, in-memory Compact works, reload returns to Comfortable, zero `role=alert`, zero page errors |
| DEN-08 | Compact persists across find filtering, Title ordering and starring; favorite text/targets stay usable |

Storage refusal traps are scoped to the density key on purpose: the library
page has other legitimate storage users (cache/favorites), and this feature
owns only its own enum. An implementation that also survives a fully
throwing `localStorage` getter passes trivially.

## Reproduce locally (repo root)

```sh
pnpm --dir apps/web install --frozen-lockfile --prod=false
pnpm --dir apps/web test src/routes/library/density.acceptance.test.ts
pnpm --dir apps/web test src/routes/appearance.acceptance.test.ts
pnpm --dir apps/web check
CI=1 pnpm --dir apps/web exec playwright test tests/library-density.acceptance.spec.ts --workers=1
```

Playwright's existing config builds and starts a local preview on 4173 and
tears it down. Ensure that port is free; `CI=1` refuses reusing a different
preview. If 4173 is occupied, use a throwaway config with an unused strict
port (same `testDir`, matching `baseURL`) and delete it afterwards. Some
hosts need `LD_LIBRARY_PATH` pointing at their provisioned Chromium
libraries. No staging, production, provider, database or secrets are
needed; specs abort non-loopback requests and fulfill `/api/library`
locally with four fixture tracks.

## Observed RED baseline (at the base above, test-author run)

- SSR bank: **2 assertion failures, 1 control pass**. DEN-SSR-01/02 both
  fail with "the real library page must server-render exactly one density
  select with Comfortable/Compact options: expected [] to have a length of
  1 but got +0" — rendering itself succeeds under the storage traps.
- Browser bank: **14 assertion failures, 1 control pass (DEN-00)**, zero
  timeouts or harness errors. Every failure is the same first assertion:
  `library page must expose a native select named "Density"`.
- `appearance.acceptance.test.ts`: **3/3 pass** unchanged. The established
  library SSR harness (`bulk-queue.acceptance.test.ts`) also passes 7/7 at
  this base.
- Full web unit suite with this bank: **2 failed / 2327 passed** (158
  files); the only failures are DEN-SSR-01/02 above.
- `pnpm --dir apps/web check`: **0 errors** (1 pre-existing
  `PaywallModal.svelte` redundant-role warning).

Next action: builder implements the contract on this branch without
modifying the protected tests, runs the paired suites plus the full web
unit suite and typecheck, then owns the combined ready PR through
same-head CI/review gates. Suspected test defects route back to the test
author; do not weaken a red assertion to fit an implementation.

## Acceptance repair note (test-author, harness only)

Three harness defects surfaced once the implementation existed; the fixes
below change test mechanics only and preserve every behavioral assertion:

- DEN-03 target identity: `rowTargets` keys now include the owning list
  (`library-item`/`queue-item`/`favorite-item`). The same track renders as
  identically named buttons in the browse list and the favorites shelf, and
  the old key aliased them, comparing one list's button against the other's
  size. A `[tabindex="-1"]` row (programmatic-focus landing spot, not an
  interactive control) is excluded from the control-target set; its height
  is the row height DEN-02 expects Compact to reduce.
- DEN-04 sequential-focus origin: each capture now blurs, then explicitly
  focuses the Density select as a common anchor before tabbing. Blur alone
  leaves Chromium's sequential focus-navigation starting point at the last
  click, which made the two captures rotations of one order instead of the
  same sequence; focusing an already-focused anchor is a no-op, hence the
  blur first. The real keyboard operation of the select and the visible
  focus-outline assertions are unchanged.
- DEN-05 audit lifetime: `addInitScript` resets the in-page write audit on
  every navigation, so writes are now accumulated outside the page realm —
  each realm's audit is collected before its reload, and the final realm is
  collected last. A reload-time rewrite would still appear in the next
  realm's fragment. The raw-enum-only and appearance-isolation assertions
  compare the same expected write list as before.

Re-verified after repair: browser bank 15/15 green against the
implementation head; SSR bank and the unchanged appearance bank green;
pre-feature base `a242bbc721f17e736757dadc4a6f85ba396e2d3f` still fails
assertion-level on the absent Density control (DEN-00 control passes).

Geometry evidence is deliberately bounded: row heights and target sizes are
live `getBoundingClientRect` measurements in Chromium against the local
preview fixture, not screenshots of a deployed build and not a claim about
any other browser/engine. Text visibility uses rendered text-range boxes
with computed visibility, so `display:none`, `visibility:hidden`, zero
opacity and near-zero font sizes all fail it.

Skills used: paperclip, test-driven-development, verification-before-completion.
