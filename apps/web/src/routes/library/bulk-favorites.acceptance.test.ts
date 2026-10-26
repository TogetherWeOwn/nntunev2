import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'svelte/server';
import type { LibraryItem } from '#lib/library/cache.js';
import type { LibraryRefreshState, LibraryRefresherDeps } from '#lib/library/refresh.js';
import { selectFavoriteItems } from '#lib/library/favorites.js';
import {
	createSelection,
	hiddenSelectedCount,
	pruneSelection,
	selectionCountLabel,
	toggleSelected
} from '#lib/library/selection.js';

/**
 * BULKSTAR acceptance bank — protected; the paired build makes these files
 * green without editing them. No standalone ready PR for this deliberately
 * RED bank. Paired interactive coverage:
 * tests/library-bulk-favorites.spec.ts.
 *
 * Reproduce (existing package scripts, from repo root):
 *   pnpm --dir apps/web check
 *   pnpm --dir apps/web test src/routes/library/bulk-favorites.acceptance.test.ts
 *   pnpm --dir apps/web test:e2e tests/library-bulk-favorites.spec.ts --workers=1
 * With this pnpm version omit the extra `--`: it makes the runners ignore
 * the focused selection/options and run the whole suite instead.
 *
 * SSR cannot run clicks/onMount. Replay a snapshot produced by the REAL
 * library refresher at its existing onStateChange boundary; keep the real
 * page, row rendering and refresher methods. Only fetch is faked. No feature
 * stub, new import/API requirement, source-text pin, or fake bulk
 * implementation. The browser half verifies starring order, counts,
 * idempotence, storage, auth, focus, themes and zero side effects.
 *
 * Breaks: absent/mislabeled Star/Unstar selected controls; controls outside
 * the selection area or below the rows; actionable empty state; missing
 * disabled reason; new select-all dependency; changed selection/favorites
 * order semantics the build reuses. U01–U02 must be assertion-RED on the
 * pre-feature app, not import/setup errors. U03–U06 are passing controls for
 * the existing selection/favorites contracts the build reuses.
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

async function renderLoaded(items: LibraryItem[]): Promise<string> {
	const real = await vi.importActual<typeof import('#lib/library/refresh.js')>(
		'#lib/library/refresh.js'
	);
	const refresher = real.createLibraryRefresher({
		sessionStorage: () => null,
		invalidateLibraryCache: () => {}
	});
	await refresher.refresh(
		async () =>
			new Response(JSON.stringify(items), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			})
	);
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

function bulkButtons(body: string, label: 'Star selected' | 'Unstar selected'): string[] {
	const buttons = [...body.matchAll(/<button\b[^>]*>[\s\S]*?<\/button>/g)].map(
		(match) => match[0]
	);
	return buttons.filter((button) => {
		const accessible =
			button.match(/aria-label="([^"]*)"/)?.[1] ??
			button
				.replace(/<[^>]*>/g, ' ')
				.replace(/\s+/g, ' ')
				.trim();
		return accessible === label;
	});
}

function requireBulkButtons(body: string): [string, string] {
	const star = bulkButtons(body, 'Star selected');
	const unstar = bulkButtons(body, 'Unstar selected');
	expect(star, 'one labeled Star selected action must render from the real page').toHaveLength(
		1
	);
	expect(unstar, 'one labeled Unstar selected action must render from the real page').toHaveLength(
		1
	);
	return [star[0]!, unstar[0]!];
}

beforeEach(() => {
	snapshot.value = null;
});

describe('selected-library bulk favorites actions', () => {
	it('BULKSTAR-U01 empty library exposes disabled bulk actions and honest empty state', async () => {
		const body = await renderLoaded([]);
		expect(body).toContain('Your library is empty');
		const [star, unstar] = requireBulkButtons(body);
		expect(star).toMatch(/\sdisabled(?:[\s=>])/);
		expect(unstar).toMatch(/\sdisabled(?:[\s=>])/);
		expect(body).not.toContain('data-testid="queue-item"');
	});

	it('BULKSTAR-U02 loaded library renders one disabled pair above the rows with a reason', async () => {
		const body = await renderLoaded([
			{ id: 'bulkstar-zulu', title: 'Zulu Song' },
			{ id: 'bulkstar-alpha', title: 'Alpha Song' }
		]);
		const [star, unstar] = requireBulkButtons(body);
		// Fresh state selects nothing, so both actions are disabled.
		expect(star).toMatch(/\sdisabled(?:[\s=>])/);
		expect(unstar).toMatch(/\sdisabled(?:[\s=>])/);
		// An understandable reason travels with each disabled control.
		for (const button of [star, unstar]) {
			const described = /aria-describedby="([^"]+)"/.exec(button)?.[1];
			const titled = /title="([^"]*)"/.exec(button)?.[1];
			if (titled && /select/i.test(titled)) continue;
			expect(
				described,
				'disabled bulk control must name its reason via title or aria-describedby'
			).toBeTruthy();
		}
		expect(body).toContain('0 selected');
		expect(body.indexOf(star)).toBeLessThan(body.indexOf('data-testid="library-list"'));
		expect(body.indexOf(unstar)).toBeLessThan(body.indexOf('data-testid="library-list"'));
		expect([...body.matchAll(/data-testid="library-item"/g)]).toHaveLength(2);
		expect(body).not.toMatch(/select all|select displayed/i);
	});
});

describe('existing selection and favorites contracts (controls, not a test-only implementation)', () => {
	it('BULKSTAR-U03 selection keeps activation order, prunes missing and counts hidden', () => {
		let selection = createSelection();
		selection = toggleSelected(selection, 'bulkstar-mike');
		selection = toggleSelected(selection, 'bulkstar-zulu');
		selection = toggleSelected(selection, 'bulkstar-mike');
		selection = toggleSelected(selection, 'bulkstar-mike');
		expect(selection).toEqual(['bulkstar-zulu', 'bulkstar-mike']);
		expect(pruneSelection(selection, ['bulkstar-mike'])).toEqual(['bulkstar-mike']);
		expect(hiddenSelectedCount(selection, ['bulkstar-zulu'])).toBe(1);
		expect(selectionCountLabel(selection.length)).toBe('2 selected');
		expect(selectionCountLabel(-1)).toBe('0 selected');
	});

	it('BULKSTAR-U04 favorites projection keeps loaded order and drops unknown ids', () => {
		const loaded: LibraryItem[] = [
			{ id: 'bulkstar-zulu', title: 'Zulu Song' },
			{ id: 'bulkstar-alpha', title: 'Alpha Song' },
			{ id: 'bulkstar-mike', title: 'Mike Song' }
		];
		const projected = selectFavoriteItems(loaded, [
			'bulkstar-mike',
			'bulkstar-absent',
			'bulkstar-zulu'
		]);
		expect(projected.map((item) => item.id)).toEqual(['bulkstar-zulu', 'bulkstar-mike']);
		expect(loaded.map((item) => item.id)).toEqual([
			'bulkstar-zulu',
			'bulkstar-alpha',
			'bulkstar-mike'
		]);
	});

	it('BULKSTAR-U05 selection toggle self-heals duplicates and rejects invalid ids', () => {
		const dirty = ['bulkstar-mike', 'bulkstar-mike', '', 42 as unknown as string];
		expect(toggleSelected(dirty, 'bulkstar-mike')).toEqual(['', 42]);
		const selection = createSelection();
		expect(toggleSelected(selection, '')).toBe(selection);
		expect(toggleSelected(selection, 42)).toBe(selection);
		// Prune drops invalid entries but never dedupes or reorders survivors.
		expect(pruneSelection(dirty, ['bulkstar-mike'])).toEqual([
			'bulkstar-mike',
			'bulkstar-mike'
		]);
	});

	it('BULKSTAR-U06 single-row star labels keep their existing toggle contract', async () => {
		const body = await renderLoaded([{ id: 'bulkstar-mike', title: 'Mike Song' }]);
		const buttons = [...body.matchAll(/<button\b[^>]*>[\s\S]*?<\/button>/g)].map(
			(match) => match[0]
		);
		const star = buttons.filter(
			(button) => button.match(/aria-label="([^"]*)"/)?.[1] === 'Star Mike Song'
		);
		expect(star, 'existing single-row star toggle must keep rendering').toHaveLength(1);
		expect(star[0]).toMatch(/aria-pressed="false"/);
	});
});
