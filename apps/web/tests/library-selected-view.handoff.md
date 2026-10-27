# Selected-only library view: independent RED handoff

Test-only branch: `test/library-selected-view`. Baseline:
`a242bbc721f17e736757dadc4a6f85ba396e2d3f`. No standalone failing PR; the
builder carries these tests unchanged into one combined ready PR. The
acceptance task's work products identify the immutable handoff SHA.

## Protected tests and consumer boundary

- `apps/web/tests/library-selected-view.spec.ts`
- `apps/web/src/lib/library/selected-view-acceptance.test.ts`

No production source or existing suite is changed. If a protected test is
incorrect, return it to the acceptance lane rather than weakening it.

The existing `Show` combobox gains one option labeled `Selected only`.
The view renders one pure projection of the canonical loaded snapshot
through the existing selected-ID set, composed with the shipped find
matcher and ordering step. The pure half locks the builder-provided seam:
`selectSelectedItems(items, selectedIds)` in the existing
`src/lib/library/selection.ts` (preferred) or a new
`src/lib/library/selected-view.ts`, keeping input-list order, exact ID
membership, same row references, and frozen-input purity. Display order
never becomes append order: Append selected keeps activation order, while
Add displayed tracks to queue consumes exactly the displayed projection.
No second selection store, persistence key, or bulk-select control.
Empty selection and find-filtered-empty are distinct states with the
existing `Show all tracks` / `Clear search` recoveries, truthful counts
through `library-find-count`, `selection-count` and
`selection-hidden-count`, and a deselect/clear focus fallback of next
surviving actionable row, then previous row, then the Show control.
Controls that remain mounted keep their focus.

## Stable cases

| Cases | Break caught |
| --- | --- |
| SV-U01–U05 | Loaded order over activation order, unknown/pruned/duplicate IDs, exact membership, immutability |
| SV-U06–U07 | Find and ordering composition of the single projection |
| SV-B-00 | Positive control: fixtures, existing Show choices and individual selection |
| SV-B-01–B04 | Noncontiguous membership, view-switch purity, ordering/find composition, empty vs filtered-empty recovery |
| SV-B-05–B06 | Display order leaking into append order; displayed action queuing hidden/unselected rows |
| SV-B-07 | Focus falling to the body after deselect/clear; focus stolen from mounted controls |
| SV-B-08 | View changes fetching, writing storage, or touching queue/current/media/favorites |
| SV-B-09–B10 | Transient refresh keep, successful-refresh prune, account teardown reset |
| SV-B-11 | 320px light/dark visibility and operability of the view's own controls |

## Reproduce from the repository root

```sh
pnpm --dir apps/web exec vitest run src/lib/library/selected-view-acceptance.test.ts
pnpm --dir apps/web test
pnpm --dir apps/web check
CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
  tests/library-selected-view.spec.ts --workers=1 --retries=0
```

`CI=1` refuses an unrelated server on 4173. If occupied, use a run-owned
config with a free port, matching 127.0.0.1 baseURL, and
`reuseExistingServer: false` instead of reusing or stopping someone else's
server. On hosts with separately unpacked Chromium libraries, supply their
directory in `LD_LIBRARY_PATH`.

## Observed on the baseline (acceptance run)

- New pure vectors: 7 intended-behavior assertion failures
  (`selectSelectedItems` undefined, naming both canonical homes),
  1 harness self-check pass. No import or collection errors.
- Full web Vitest: 2327 passes and exactly those 7 new failures
  (158 files, 2334 tests); every pre-existing test still passes.
- `pnpm --dir apps/web check`: 0 errors (1 pre-existing warning in an
  unrelated component). A standalone strict `tsc` of the new browser spec
  also passes, and Playwright discovery lists all 12 browser cases.
- Browser execution was not run on the authoring host: Chromium cannot
  launch there (`libglib-2.0.so.0` missing, no shared dependency bundle),
  which is a tooling absence, not RED evidence. The builder must run the
  browser bank above (and it gates the combined PR in hosted `web-e2e`);
  on the baseline every SV-B-01…B-11 case is gated to fail on the missing
  `Selected only` option assertion, with SV-B-00 as the passing control,
  once a working Chromium is supplied.

Neighboring suites pinned to stay green in the combined PR include
`library-subset-append`, `library-local-filter`, `library-find-reliability`,
`library-order-direction`, `library-bulk-visible-builder`,
`library-favorites-management` (and its regression spec),
`library-selectable` and `library-selection-isolation`. No live provider,
production request, deployment, spend or secret is part of this handoff;
mocked local fixtures do not prove live streaming or deployment.
