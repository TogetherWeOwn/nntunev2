# Web per-route code splitting plan

Plan for keeping the web client inside its bundle budget as route-level
features land. This document changes no code; it records the measured
starting point, the splitting rules a new route or heavy feature must
follow, and the order to apply them in.

## The constraint

CI fails the build when total client JS exceeds 368,640 raw bytes
(`.github/scripts/check_web_bundle_budget.py`, run by the `web-build`
job; see the Bundle budget section of `apps/web/README.md`). The historical
2026-11-03 baseline was 314,730 B across 37 files, leaving 53,910 B
of headroom under this ceiling. The W10 app-shell measurement is
339,150 B across 52 files, leaving 29,490 B of headroom.
The gate sums all emitted client JS, so moving code into a lazy chunk
alone does not reduce that total. Lazy loading still reduces the bytes
loaded before the corresponding interaction or route is needed.

## Where the bytes are today

Largest client files from the historical 314,730 B baseline (raw bytes):

| File | Bytes | What it is |
| --- | ---: | --- |
| `nodes/9.*.js` | 61,957 | `/library` page |
| `nodes/8.*.js` | 52,459 | `/demo/player` page (retired: route removed, EL TWO-5795) |
| `chunks/CN88kLYU.js` | 42,171 | Svelte runtime (shared, unavoidable) |
| `chunks/BBY8yOdC.js` | 41,850 | Code shared between the player surfaces |
| `nodes/12.*.js` | 18,699 | `/onboarding/byop` page |
| `nodes/4.*.js` | 9,601 | `/account/billing` page |
| `nodes/5.*.js` | 8,497 | `/account/provider` page |
| everything else | ≤ 7,118 each | Root layout, remaining pages, small chunks |

SvelteKit already emits one chunk per route node, so per-route splitting
at the page level needs no bundler configuration: the risk is not page
chunks, it is heavy code pulled into shared chunks or into the root
layout (`apps/web/src/routes/+layout.svelte`), which every route loads.
Today only two dynamic imports exist (both for the billing client in
`apps/web/src/routes/account/billing/+page.svelte`), and `apps/web/vite.config.ts`
sets no `manualChunks`.

## Rules for new or growing code

1. **Route-local beats shared.** Code used by one route lives under that
   route's directory (or a `$lib` module imported only by it) until a
   second route needs it. Never import route-specific code from the root
   layout, `apps/web/src/app.css`, or a shell component.
2. **Lazy-load interaction-gated features.** Anything only needed after a
   user action (wizards, dialogs, parsers, export/share helpers) loads
   with a dynamic `import()` at the point of use, following the billing
   client precedent. Prefer this over growing the route's eager chunk.
3. **Watch the shared-chunk cliff.** A module imported by two routes
   moves into a shared chunk that both routes pay for; a module imported
   by the layout is paid by all routes. Before adding an import to a
   shared module, check whether the import is the heavy part and keep it
   behind a dynamic import instead.
4. **No manualChunks yet.** Hand-tuned chunk groups add cache-invalidation
   and preload side effects. Revisit only if a shared chunk exceeds the
   Svelte runtime chunk (~42 KB) and rules 1–3 cannot shrink it; any
   change needs a before/after measurement in the pull request.
5. **Measure in the pull request.** Push the PR branch (draft while
   incomplete) and use the exact-head Actions `web-build` job, which runs
   the build and `check_web_bundle_budget.py`. Link the job and record the
   new total and headroom; do not mirror the build gate locally. Follow
   [verification policy](../CONTRIBUTING.md#verification-github-actions-first).
   A change that halves the remaining headroom needs a splitting note like
   this one.

## Order of work when headroom runs low

1. Move the `/library` surface's interaction-only panels (queue management
   extras, recently-played actions) behind dynamic imports; it remains the
   largest route chunk. (`/demo/player` was retired — see the table above.)
2. Split the BYOP onboarding wizard steps so only the active step is
   eager (18.7 KB today).
3. Audit the shared player chunk: keep playback-critical code eager,
   lazy-load display-only helpers.
4. Only then consider raising the budget, with the measurement history in
   `apps/web/README.md` updated in the same change.

## Related gates

- Accessibility: `apps/web/tests/accessibility-axe.spec.ts` (axe, zero
  serious/critical per route) runs in the `web-a11y` CI job and inside
  the full `web-e2e` suite. Splitting must not hide content from that
  scan; lazy chunks still render before the scan's readiness checks.
- Visual screenshot baselines are deferred until the next shell rebuild;
  when they land, add them per route so a split does not silently change
  first paint.
