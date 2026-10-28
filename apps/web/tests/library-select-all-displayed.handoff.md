# Select all displayed tracks: independent RED handoff

Test-only branch: `test/library-select-all-displayed`. Baseline:
`d2b4ff9bba69b65bb97edf1290367dc1aec951b1`. No standalone failing PR; the
builder carries these tests unchanged into one combined ready PR. The
acceptance task's work products identify the immutable handoff SHA.

## Protected tests and consumer boundary

- `apps/web/tests/library-select-all-displayed.spec.ts`
- `apps/web/src/lib/library/select-all-displayed-acceptance.test.ts`

If a protected test is incorrect, return it to the acceptance lane rather
than weakening it.

The library page gains one button with the exact accessible name
`Select all`, inside `main`, offered while library rows are displayed. It
selects exactly the currently displayed set — the visible rows after the
existing Show view, ordering control, find field and Search in scope —
never the whole loaded snapshot and never anything hidden. The pure half
locks the builder-provided seam: `selectAllDisplayed(selection,
displayedIds)` in the existing `src/lib/library/selection.ts`, the module
that already owns `toggleSelected` / `clearSelection` / `pruneSelection` /
`hiddenSelectedCount` (selection first, ID list second, like
`pruneSelection`). It keeps valid pre-existing selections in activation
order (first occurrence wins), appends newly displayed IDs in displayed
order, is additive-only (valid hidden selections survive; an ID absent
from the displayed list is never added, so a pruned ID cannot be
resurrected), uses exact case-sensitive membership, ignores invalid
entries in either input without throwing, and never mutates its inputs.
Append selected therefore queues a select-all in displayed order.

`Clear selection` keeps emptying everything; `selection-count` and
`selection-hidden-count` stay truthful. The new control is enabled while
rows are displayed (a redundant activation is a harmless no-op, mirroring
the always-enabled Clear button), keyboard-reachable by Tab, at least
24x24 CSS px, keeps focus when activated, and adds no assertive live
region or alert inside `main`. Per-row checkbox semantics do not change;
deselect-a-few after a select-all just works.

Two earlier banks pinned the *absence* of any Select all affordance for
their slices. Those assertions are retired in this same change (comment
notes left in place): `apps/web/tests/library-subset-append.spec.ts`
(SUB-B-01) and `apps/web/tests/library-selected-view.spec.ts` (SV-B-01).
No other existing case or assertion changes, and no production source is
touched.

## Stable cases

| Cases | Break caught |
| --- | --- |
| SA-U01–U02 | Displayed-order append behind existing activation order |
| SA-U03–U05 | Displayed-subset-only add, hidden preservation, truthful counts, no duplicates, exact case-sensitive membership |
| SA-U06–U08 | Invalid entries ignored in either input, frozen-input purity, empty inputs |
| SA-U09–U11 | Prune composition never resurrects, find-scope/ordering composition follows the newly displayed set, no globals |
| SA-B-00 | Positive control: fixtures, existing controls and individual selection |
| SA-B-01–B03 | One-action select-all plus deselect-a-few, clear and re-select, find-subset select-all with hidden preservation |
| SA-B-04–B05 | Search-scope switch follows the newly displayed set; ordering switch append order proven through Append selected |
| SA-B-06 | Successful-refresh prune; selecting-all again never resurrects the pruned ID |
| SA-B-07 | Tab reachability, 24x24 target, focus survival, polite announcements inside `main` |
| SA-B-08–B09 | Presentation-only side-effect audit; Selected-only review shows exactly the scoped select-all |

## Reproduce from the repository root

```sh
pnpm --dir apps/web exec vitest run src/lib/library/select-all-displayed-acceptance.test.ts
pnpm --dir apps/web test
pnpm --dir apps/web check
CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
  tests/library-select-all-displayed.spec.ts --workers=1 --retries=0
```

`CI=1` refuses an unrelated server on 4173. If occupied, use a run-owned
config with a free port, matching 127.0.0.1 baseURL, and
`reuseExistingServer: false` instead of reusing or stopping someone else's
server. On hosts with separately unpacked Chromium libraries, supply their
directory in `LD_LIBRARY_PATH`.

## Observed on the baseline (acceptance run)

- New pure vectors: 11 intended-behavior assertion failures
  (`selectAllDisplayed` undefined, naming the locked home), 2 harness
  self-check passes. No import or collection errors.
- New browser bank, executed locally against a real Chromium: 9 failures,
  each the same missing-control count assertion in `requireSelectAll`
  (expected one `Select all` button in `main`, received 0), plus the
  SA-B-00 control pass.
- Full web Vitest: 2349 passes and exactly those 11 new failures
  (160 files, 2360 tests); every pre-existing test still passes.
- `pnpm --dir apps/web check`: 0 errors (1 pre-existing warning in an
  unrelated component). A standalone strict `tsc` of the new browser spec
  also passes, and Playwright discovery lists all 10 browser cases.
- Achievability was verified in a throwaway scratch copy of `apps/web`
  (the candidate contract above, applied minimally): all 13 pure vectors
  and all 10 browser cases pass, along with the full
  `library-subset-append` and `library-selected-view` banks and the
  neighbouring `library-find-scope`, `library-find-reliability`,
  `library-local-filter`, `library-bulk-queue`,
  `library-bulk-visible-builder`, `library-selectable`,
  `library-selection-isolation` and `library-order-direction` specs
  (107 browser tests across the two scratch runs). The scratch copy was
  discarded; nothing from it ships here.

Neighboring suites pinned to stay green in the combined PR include
`library-subset-append`, `library-selected-view`, `library-local-filter`,
`library-find-reliability`, `library-find-scope`, `library-order-direction`,
`library-bulk-queue`, `library-bulk-visible-builder`, `library-selectable`
and `library-selection-isolation`. No live provider, production request,
deployment, spend or secret is part of this handoff; mocked local fixtures
do not prove live streaming or deployment.
