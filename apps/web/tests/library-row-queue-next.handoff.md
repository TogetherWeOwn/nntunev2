# Library row Queue next: independent RED handoff

Test-only branch: `test/library-row-queue-next`. Baseline:
`d2b4ff9bba69b65bb97edf1290367dc1aec951b1`. No standalone failing PR; the
builder carries these tests unchanged into one combined ready PR. The
acceptance task's work products identify the immutable handoff SHA.

## Protected tests and consumer boundary

- `apps/web/tests/library-row-queue-next.spec.ts`

No production source or existing suite is changed. If a protected test is
incorrect, return it to the acceptance lane rather than weakening it.

The library route gains one `button[type=button]` per loaded row with the
exact accessible name `Queue next <display title>` and visible `Queue next`
text, plus a `queue-next-result` (`role="status"`) one-shot region with the
exact texts the spec pins (inserted/moved/already-next/current/
no-longer-loaded, never a playing claim). The activation resolves the row by
current loaded stable ID through the existing stable-ID queue helpers: a
missing track is inserted immediately after current, an already-queued
noncurrent track moves there without duplication, the current track is a
no-op. Current ID/source and surviving relative order survive; an empty
queue gains one selected metadata-only row, never audible playback. Moved
rows keep their queued metadata; inserted rows take current loaded
metadata. No play/load/seek, stream fetch/renewal, favorites or selection
mutation, auto-advance, or playback promise. Refresh and auth teardown
reconcile stale feedback; focus stays on the initiating surviving action
with no scroll theft. Row append, star, copy link, find/views and the
queue-panel Play next stay unchanged.

## Stable cases

| Cases | Break caught |
| --- | --- |
| QN-B-00 | Positive control: fixtures render in API order, queue empty |
| QN-B-01 | Missing/unlabeled/duplicated per-row Queue next action |
| QN-B-02 | Empty queue not gaining exactly one selected metadata-only row |
| QN-B-03–B05 | Insert-after-current, move-from-after, move-from-before vectors |
| QN-B-06–B07 | Current and already-next no-ops changing state or feedback |
| QN-B-08 | Repeated activation duplicating rows, badges or messages |
| QN-B-09 | Activation leaking into selection, star, copy or find state |
| QN-B-10–B11 | Keyboard/touch activation losing focus or scrolling |
| QN-B-12–B13 | Filtered and favorites-only rows failing or mutating views |
| QN-B-14–B16 | Refresh/auth teardown leaving stale queue entries or feedback |
| QN-B-17 | Activation fetching, streaming or mutating over the network |
| QN-B-18 | Narrow light/dark hiding or breaking the action |
| QN-B-19–B20 | Inserted rows missing loaded metadata; moved rows losing queued metadata |
| QN-B-21 | Row append, star, copy or panel Play next regressions |
| QN-B-22 | Playback claims in feedback; duplicated or missing Queued badges |

## Reproduce from the repository root

```sh
pnpm --dir apps/web check
pnpm --dir apps/web test
pnpm --dir apps/web exec playwright test \
  tests/library-row-queue-next.spec.ts tests/library-queue-manage.spec.ts
```

Do not pass extra `--` args through a script alias: scope specs positionally
via `pnpm exec` as above, or everything runs. If port 4173 is occupied by an
unrelated preview, use a run-owned config with a free port, matching
127.0.0.1 baseURL, and `reuseExistingServer: false` instead of reusing or
stopping someone else's server. On hosts with separately unpacked Chromium
libraries, supply their directory in `LD_LIBRARY_PATH`.

## Observed on the baseline (acceptance run)

- New browser bank: 22 intended-behavior assertion failures (missing
  `Queue next <title>` control; `element(s) not found` on the guarded
  visibility assertions), 1 control pass (QN-B-00). No import, collection,
  timeout or browser-launch errors.
- Full web Vitest: 2347 passes (159 files); every pre-existing test still
  passes. One early full-suite invocation reported 3 failures that cleared
  on the immediate rerun against the unchanged tree (worker-startup flake).
- `pnpm --dir apps/web check`: 0 errors (1 pre-existing warning in the
  unrelated billing PaywallModal).
- Neighbor `library-queue-manage`: 13/13 green alongside the new bank.

Neighboring suites pinned to stay green in the combined PR include
`library-queue-manage`, `library-subset-append`, `library-player-queue`
(and its pins spec), `library-queue-selection-regressions`,
`library-queue-star` (and its regressions) and `library-favorites-queue-all`.
No live provider, production request, deployment, spend or secret is part of
this handoff; mocked local fixtures do not prove live streaming.
