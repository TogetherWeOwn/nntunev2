# Appearance acceptance handoff

Test-only, deliberately RED. Base: `9b034604bad8c489ee71effa7544132fe5017dfe`.
Handoff ref: `test/appearance-acceptance`; the test card records the exact pushed SHA.
The builder consumes this commit in the **one combined implementation PR**; do not
open a separate knowingly-red test PR. No production source/config was changed.

## Protected files and consumer contract

Preserve these files; implement the app, not replacement test behavior:

- `src/routes/appearance.acceptance.test.ts` — real root-layout SSR (existing default export).
- `tests/appearance.helpers.ts` — local-only fixtures, token and runtime assertions.
- `tests/appearance-preference.spec.ts` — deterministic preference/storage cases.
- `tests/appearance-runtime.spec.ts` — real DOM, keyboard, OS, navigation, paint/CSP.

Paths are relative to `apps/web`. No new production function export or test-only
production seam is required. Use the existing root layout; no settings route.
Smallest UI contract: a visible native select labeled **Appearance**, with options
**System**, **Light**, **Dark**, values `system`, `light`, `dark`, in that order.
System is the SSR/default choice. Store only a raw validated enum under
`nntune:appearance:v1` in localStorage; no identity, cookie, sessionStorage or HTTP
write. Unknown, padded, mixed-case, JSON, blank and missing values mean System.
Storage getter/read/write refusal must preserve a usable UI and in-memory choices.

Existing nine color tokens remain the palette source. The browser checks their
resolved values plus body colors, native-control color-scheme, and reduced motion.
Explicit overrides constrain the native scheme; System may retain UA light/dark
negotiation. The existing visible focus outline must contrast against the page.
No root attribute, class or module name is prescribed.

## Stable case families

| IDs | Observable protection |
| --- | --- |
| APP-SSR-01..03 | Three choices/default; no browser access during SSR; children/PWA head preserved |
| APP-PREF-01-{OS}-{enum} | All validated choices under both OS palettes |
| APP-PREF-02-{OS}-{fixture} | Strict enum validation and live System fallback (11 fixtures × 2) |
| APP-PREF-03-{getter,read,write} | Refused storage, mounted OS change, live override, reload fallback |
| APP-PREF-04..05 | All choices reload, including return to System; enum-only/device-only persistence |
| APP-DOM-01/02-{OS}[-{enum}] | Accessible/visible state, palette and native controls; fixed override vs live OS |
| APP-A11Y-01-{OS} | Actual Tab order, visible focus, Home/End/ArrowDown keyboard selection |
| APP-PAINT-01-{enum} | Saved override opposite OS at the first revealed app frame, not just final DOM |
| APP-LIFE-01 | Live OS changes; explicit preference survives client navigation; repeated choices/navigation retain document and listener baseline |
| APP-REG-01..03 | Strict CSP, unchanged PWA metadata, reduced motion; existing live OS palette regression guard |

No synthetic unit preference implementation: the existing Node Vitest harness
renders the real layout; validation and mount/navigation lifecycle are exercised
through the browser DOM because the repo has no client-DOM Vitest harness. Media
listener instrumentation forwards real matchMedia methods (including legacy and
onchange); it never fakes palette application. Full document reload tests repeated
startup, while client navigation checks listener stability within the same document.

## Reproduce locally (repo root)

```sh
pnpm --dir apps/web install --frozen-lockfile
pnpm --dir apps/web test src/routes/appearance.acceptance.test.ts
pnpm --dir apps/web exec playwright test tests/appearance-preference.spec.ts tests/appearance-runtime.spec.ts --workers=1
pnpm --dir apps/web exec playwright test tests/pwa-manifest.spec.ts tests/security-headers.spec.ts tests/pricing-blurb-contrast.spec.ts tests/web-shell-a11y-runtime.spec.ts --workers=1
pnpm --dir apps/web check
pnpm --dir apps/web test
```

Playwright's existing config builds and starts a local preview on 4173 and tears it
down. Ensure that port is free; use `CI=1` to refuse reusing a different preview.
No staging, production, provider, database or secrets are needed. Appearance suites
abort non-loopback requests and fulfill API attempts locally; the privacy case also
asserts that the UI made no such requests. Chromium must already be installed with
its system libraries. A sandbox with separately provisioned Chromium libraries may
need its existing `LD_LIBRARY_PATH`; no dependency or CSP change is authorized.

## Observed RED baseline

- Before these tests: Vitest **1655/1655 pass** (107 files).
- With acceptance: **1 assertion failure, 1657 pass** (108 files). APP-SSR-01
  expects a server-rendered select; actual select markup is empty. SSR import,
  rendering, children and PWA guards execute successfully; no missing module/stub.
- `pnpm --dir apps/web check`: **0 errors, 0 warnings**.
- Browser: **48 intended assertion failures, 1 new guard pass**, plus **27/27
  existing controls pass**; no skipped/flaky cases or harness errors. Failures:
  44 absent-selector assertions, 2 wrong saved palettes, 2 first-frame mismatches.
  All 49 new browser cases show a rendered main; captured CSP violations: zero.
  Saved Light under dark OS first reveals `#0b0d12` (expected `#f7f8fb`); saved
  Dark under light OS reveals the reverse. The existing live OS guard passes.
- The privacy case was re-run after adding fetch/XHR detection for arbitrary
  backend paths: same absent-selector assertion, no import/startup error.
- Exact pushed commit, JSON results and browser screenshots are on the test card.
  These are RED evidence, not a claim that deeper feature assertions ran or passed.

Next action: builder implements the contract without modifying protected tests,
runs the paired suites and existing regression commands, then owns the combined
ready PR through same-head CI/review gates. Do not weaken a red assertion or CSP.

First-paint evidence is deliberately bounded: the init-script observer records
requestAnimationFrame samples once application content is renderable, with JSON
samples and screenshots in Playwright output. It is not a pre-CSS pixel filmstrip,
not a claim about live production, and not proof that JavaScript-disabled rendering
can read device storage. Record any real first-paint limitation; do not conceal it
with an eventual-state-only assertion, unsafe-inline, or a relaxed security header.

Skills used: paperclip, paperclip-superpowers-adapter, test-driven-development, verification-before-completion.
