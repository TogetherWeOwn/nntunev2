import { describe, expect, it } from 'vitest';

/**
 * Acceptance: truthful local NZB selection feedback (import echo bundle).
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the route
 * changes on top of this branch and must make these tests pass WITHOUT
 * editing this file; a wrong test comes back to the acceptance card instead.
 * The browser half is `apps/web/tests/import-echo.spec.ts`.
 *
 * What this file pins (fast, offline, no browser):
 * - the import surface renders its requirements hint and size echo from the
 *   single exported cap `MAX_NZB_IMPORT_BYTES` — no forked byte literal, no
 *   second cap constant in route/component markup. The validator half
 *   (`./import-validation.test.ts`) already pins the cap value (2 MiB,
 *   mirroring `MAX_NZB_BYTES` in `workers/ingest/src/nzb_intake.rs`) and the
 *   accept/reject boundaries; this file only pins that the UI reuses that
 *   single source instead of forking it.
 * - (The browser spec's own reuse of the single source for its boundary
 *   buffers is self-pinned inside the spec as IMPORTECHO-00: `tests/` is
 *   not visible to a Vite glob from `src/`, so it cannot be pinned here.)
 *
 * Exact hint wording, size formatting ("184 KiB" vs "188,416 bytes") and the
 * Clear button label are the builder's: the browser half pins the outcome
 * and the topic, not the literal text.
 *
 * Focused command (repo-local, deterministic, offline):
 *   pnpm --dir apps/web exec vitest run src/lib/library/import-echo-acceptance.test.ts
 *
 * NOTE: this file deliberately does not import the cap value — it pins the
 * UI's reuse of the single source by name, so a renamed export fails here
 * as an assertion (send it back to the acceptance card), never as an
 * import error. The browser spec imports the value for its boundary
 * buffers (verified working in the Playwright runner).
 */

// Raw markup via Vite glob (same seam as `./import-validation.test.ts`):
// route sources plus any lib Svelte component, so the builder may place the
// hint/echo in the import route or in a shared component without breaking
// these pins. Plain `.ts` lib files are out of scope: the validator module
// is where the single source lives.
const routeSources = import.meta.glob('../../routes/**/*.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;
const libComponentSources = import.meta.glob('../**/*.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

const uiMarkup = [...Object.values(routeSources), ...Object.values(libComponentSources)].join('\n');

describe('import echo: single size-cap source', () => {
	it('references MAX_NZB_IMPORT_BYTES in the import UI markup (no forked source)', () => {
		expect(
			routeSources['../../routes/library/import/+page.svelte'],
			'expected the import route source to load (glob seam check)'
		).toBeDefined();
		expect(
			uiMarkup,
			'expected the import UI to reference the single exported cap ' +
				'MAX_NZB_IMPORT_BYTES (import-validation.js) for its hint/echo'
		).toContain('MAX_NZB_IMPORT_BYTES');
	});

	it('defines no forked cap literal or second cap constant in the import UI markup', () => {
		expect(uiMarkup, 'no decimal byte literal fork of the cap').not.toMatch(/2097152/);
		expect(uiMarkup, 'no arithmetic fork of the cap').not.toMatch(/2\s*\*\s*1024\s*\*\s*1024/);
		expect(
			uiMarkup,
			'no second UPPER_SNAKE cap constant (reuse the single source)'
		).not.toMatch(/const\s+\w*(?:MAX|CAP|LIMIT|BYTES)\w*\s*=/);
	});
});
