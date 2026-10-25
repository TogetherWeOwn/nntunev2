import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'svelte/server';
import type { LibraryItem } from '#lib/library/cache.js';
import type { LibraryRefreshState, LibraryRefresherDeps } from '#lib/library/refresh.js';
import {
	addTrack, createQueue, currentTrack, selectTrack, type Track
} from '#lib/player/queue.js';

/**
 * BULK acceptance bank — protected; the paired build makes these files green
 * without editing them. No standalone ready PR for this deliberately RED bank.
 * Paired interactive coverage: tests/library-bulk-queue.spec.ts.
 * RED verified 2026-10-24 against adaeac2ad6d2d1967679521618a00e3db60cf62b:
 * U01–03 fail because the real rendered page has no labeled bulk action;
 * U04–07 pass. Full web unit run: 3 failed / 1867 passed (122 files).
 * Web check: 0 errors / 0 warnings. No production source changes.
 *
 * Reproduce (existing package scripts, from repo root):
 *   pnpm --dir apps/web check
 *   pnpm --dir apps/web test src/routes/library/bulk-queue.acceptance.test.ts
 *   pnpm --dir apps/web test:e2e tests/library-bulk-queue.spec.ts --workers=1
 * With this pnpm version omit the extra `--`: it makes the runners ignore
 * the focused selection/options and run the whole suite instead.
 *
 * SSR cannot run clicks/onMount. Replay a snapshot produced by the REAL library
 * refresher at its existing onStateChange boundary; keep the real page, queue,
 * row rendering and refresher methods. Only fetch is faked. No feature stub,
 * new import/API requirement, source-text pin, or fake bulk implementation.
 * The browser half verifies ordered append/count/idempotence/auth/lifecycle.
 *
 * Breaks: absent/mislabeled bulk control; actionable empty view; control below
 * rows; duplicate replacement; stolen current selection; changed stream fields.
 * U01–03 must be assertion-RED on the pre-feature app, not import/setup errors.
 * U04–07 are passing controls for the existing append semantics the build reuses.
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

function bulkButton(body: string): string {
	const buttons = [...body.matchAll(/<button\b[^>]*>[\s\S]*?<\/button>/g)].map((match) => match[0]);
	const matching = buttons.filter((button) => {
		const label = button.match(/aria-label="([^"]*)"/)?.[1]
			?? button.replace(/<[^>]*>/g, ' ').replace(/\s+/g, ' ').trim();
		return label === 'Add displayed tracks to queue';
	});
	expect(matching, 'one labeled bulk queue action must render from the real page').toHaveLength(1);
	return matching[0]!;
}

const A: Track = { id: 'bulk-a', title: 'Alpha', artist: 'Band', streamUrl: '' };
const B: Track = { id: 'bulk-b', title: 'Bravo', artist: '', streamUrl: '' };
const C: Track = { id: 'bulk-c', title: 'Charlie', artist: '', streamUrl: '' };

beforeEach(() => { snapshot.value = null; });

describe('displayed-library bulk queue action', () => {
	it('BULK-U01 empty library exposes a disabled action and honest empty state', async () => {
		const body = await renderLoaded([]);
		expect(body).toContain('Your library is empty');
		expect(bulkButton(body)).toMatch(/\sdisabled(?:[\s=>])/);
		expect(body).not.toContain('data-testid="queue-item"');
	});

	it.each([
		{ id: 'BULK-U02 singleton', items: [{ id: 'one', title: 'One' }] },
		{ id: 'BULK-U03 many', items: [{ id: 'z', title: 'Zulu' }, { id: 'a' }, { id: 'm', title: 'Middle' }] }
	])('$id enables one bulk action above the real displayed rows', async ({ items }) => {
		const body = await renderLoaded(items);
		const button = bulkButton(body);
		expect(button).not.toMatch(/\sdisabled(?:[\s=>])/);
		expect(body.indexOf(button)).toBeLessThan(body.indexOf('data-testid="library-list"'));
		expect([...body.matchAll(/data-testid="library-item"/g)]).toHaveLength(items.length);
		expect(button).not.toMatch(/play all|streaming|playing/i);
	});
});

describe('existing queue append contract (controls, not a test-only bulk implementation)', () => {
	it('BULK-U04 empty queue has no current selection; first metadata add selects index zero', () => {
		const empty = createQueue([]);
		expect(empty).toEqual({ tracks: [], currentIndex: -1 });
		expect(currentTrack(empty)).toBeNull();
		const next = addTrack(empty, A);
		expect(next).toEqual({ tracks: [A], currentIndex: 0 });
		expect(empty).toEqual({ tracks: [], currentIndex: -1 });
	});

	it('BULK-U05 repeated existing ID preserves metadata, arrangement and current index', () => {
		const before = selectTrack(createQueue([B, A]), 1);
		const next = addTrack(before, { ...B, title: 'Do not replace existing metadata' });
		expect(next).toBe(before);
		expect(next.tracks).toEqual([B, A]);
		expect(next.currentIndex).toBe(1);
	});

	it('BULK-U06 appending a missing ID never steals a non-first current selection', () => {
		const before = selectTrack(createQueue([B, A]), 1);
		Object.freeze(before.tracks);
		Object.freeze(before);
		const next = addTrack(before, C);
		expect(next.tracks.map((track) => track.id)).toEqual(['bulk-b', 'bulk-a', 'bulk-c']);
		expect(next.currentIndex).toBe(1);
		expect(currentTrack(next)).toBe(A);
		expect(before.tracks).toEqual([B, A]);
		expect(next.tracks.map((track) => track.streamUrl)).toEqual(['', '', '']);
	});

	it('BULK-U07 many existing appends retain input order while skipping mixed duplicate IDs', () => {
		let queue = selectTrack(createQueue([C, B]), 1);
		queue = addTrack(queue, B);
		queue = addTrack(queue, A);
		queue = addTrack(queue, C);
		expect(queue.tracks.map((track) => track.id)).toEqual(['bulk-c', 'bulk-b', 'bulk-a']);
		expect(queue.currentIndex).toBe(1);
		expect(currentTrack(queue)).toBe(B);
	});
});
