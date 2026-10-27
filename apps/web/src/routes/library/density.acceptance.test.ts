import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'svelte/server';
import type { LibraryItem } from '#lib/library/cache.js';
import type { LibraryRefreshState, LibraryRefresherDeps } from '#lib/library/refresh.js';

/**
 * DENSITY SSR acceptance bank — protected; the paired build makes these files
 * green without editing them. No standalone ready PR for this deliberately RED
 * bank. Paired browser coverage: tests/library-density.acceptance.spec.ts.
 * Contract and handoff: tests/library-density.acceptance.md.
 *
 * Consumer contract (smallest UI, mirroring the appearance preference):
 * the real library page server-renders one native select labeled "Density"
 * with options Comfortable/Compact (values `comfortable`/`compact`, in that
 * order) and Comfortable selected. The server render must not read browser
 * globals or device storage; a validated device-local enum may only be
 * applied on the client. No new production export or test-only seam required.
 *
 * SSR cannot run clicks/onMount. Replay a snapshot produced by the REAL
 * library refresher at its existing onStateChange boundary (the established
 * pattern in bulk-queue.acceptance.test.ts); only fetch is faked. No feature
 * stub, source-text pin, or fake density implementation.
 * DEN-SSR-01/02 must be assertion-RED on the pre-feature app (no density
 * select renders), never import/setup errors. DEN-SSR-03 is a passing
 * control for the existing page content the build must preserve.
 *
 * Reproduce (existing package scripts, from repo root):
 *   pnpm --dir apps/web test src/routes/library/density.acceptance.test.ts
 *   pnpm --dir apps/web test src/routes/appearance.acceptance.test.ts
 */

const snapshot = vi.hoisted(() => ({ value: null as LibraryRefreshState | null }));

vi.mock('#lib/library/refresh.js', async (importOriginal) => {
	const real = await importOriginal<typeof import('#lib/library/refresh.js')>();
	return {
		...real,
		createLibraryRefresher(deps: LibraryRefresherDeps) {
			const refresher = real.createLibraryRefresher(deps);
			if (snapshot.value) deps.onStateChange?.(snapshot.value);
			return refresher;
		}
	};
});

import Library from './+page.svelte';

const ITEMS: LibraryItem[] = [
	{ id: 'den-alpha', title: 'Alpha Waves', artist: 'The Densities' },
	{ id: 'den-beta', title: 'Beta Lines', artist: 'Compact Club' }
];

async function renderLoaded(items: LibraryItem[]): Promise<string> {
	const real = await vi.importActual<typeof import('#lib/library/refresh.js')>('#lib/library/refresh.js');
	const refresher = real.createLibraryRefresher({
		sessionStorage: () => null,
		invalidateLibraryCache: () => {}
	});
	await refresher.refresh(async () => new Response(JSON.stringify(items), {
		status: 200, headers: { 'Content-Type': 'application/json' }
	}));
	expect(refresher.state.loading, 'real refresher must finish before assertions').toBe(false);
	expect(refresher.state.loadError, 'fixture must pass production library validation').toBeNull();
	expect(refresher.state.items).toEqual(items);
	snapshot.value = refresher.state;
	try {
		return render(Library).body;
	} finally {
		refresher.dispose();
	}
}

function densitySelect(body: string): string {
	const selects = [...body.matchAll(/<select\b[^>]*>[\s\S]*?<\/select>/g)].map((match) => match[0]);
	const matching = selects.filter((select) =>
		/<option\b[^>]*value="comfortable"[^>]*>\s*Comfortable\s*<\/option>/.test(select) &&
		/<option\b[^>]*value="compact"[^>]*>\s*Compact\s*<\/option>/.test(select));
	expect(matching, 'the real library page must server-render exactly one density select with Comfortable/Compact options').toHaveLength(1);
	return matching[0]!;
}

beforeEach(() => { snapshot.value = null; });
afterEach(() => vi.unstubAllGlobals());

describe('track-list density SSR acceptance', () => {
	it('DEN-SSR-01: offers Comfortable and Compact with Comfortable selected on the server', async () => {
		const body = await renderLoaded(ITEMS);
		const select = densitySelect(body);
		expect(select.indexOf('value="comfortable"'), 'Comfortable is listed before Compact').toBeLessThan(select.indexOf('value="compact"'));
		expect(select).toMatch(/<option\b(?=[^>]*value="comfortable")(?=[^>]*\bselected(?:\s|=|>))[^>]*>/);
		expect(select).not.toMatch(/<option\b(?=[^>]*value="compact")(?=[^>]*\bselected(?:\s|=|>))[^>]*>/);
		// The control carries its visible "Density" label in the server markup:
		// either aria-label on the select or a <label for> naming it.
		const selectTag = select.match(/<select\b[^>]*>/)?.[0] ?? '';
		const id = selectTag.match(/\bid="([^"]+)"/)?.[1];
		const labeled = /aria-label="Density"/.test(selectTag) ||
			(id !== undefined && new RegExp(`<label\\b[^>]*for="${id}"[^>]*>[^<]*Density`).test(body)) ||
			/<label\b[^>]*>[^<]*Density[\s\S]*?<\/label>/.test(body.slice(0, body.indexOf(select)).slice(-200));
		expect(labeled, 'density select must be labeled "Density" in server markup').toBe(true);
	});

	it('DEN-SSR-02: renders Comfortable without reading browser globals or device storage', async () => {
		// Model Node SSR (no window/document); any storage method call during
		// render throws, so an unguarded density read cannot slip through.
		vi.stubGlobal('window', undefined);
		vi.stubGlobal('document', undefined);
		const storageTrap = new Proxy({}, {
			get: () => () => { throw new Error('storage access during SSR'); }
		});
		vi.stubGlobal('localStorage', storageTrap);
		vi.stubGlobal('sessionStorage', storageTrap);
		const body = await renderLoaded(ITEMS);
		expect(body).toContain('Alpha Waves');
		const select = densitySelect(body);
		expect(select).toMatch(/<option\b(?=[^>]*value="comfortable")(?=[^>]*\bselected(?:\s|=|>))[^>]*>/);
	});

	it('DEN-SSR-03 control: existing rows keep title and artist text in server markup', async () => {
		const body = await renderLoaded(ITEMS);
		expect([...body.matchAll(/data-testid="library-item"/g)]).toHaveLength(ITEMS.length);
		for (const item of ITEMS) {
			expect(body).toContain(item.title);
			expect(body).toContain(item.artist);
		}
	});
});
