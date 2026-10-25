import { describe, expect, it } from 'vitest';

import type { LibraryItem } from './cache.js';
import { displayArtist, displayTitle } from './order.js';
import {
	addTrack,
	createQueue,
	currentTrack,
	type QueueState,
	type Track
} from '../player/queue.js';

/**
 * RED acceptance for the explicit library-subset selection helpers.
 *
 * OWNERSHIP: Test-Author owned. The paired builder implements
 * `apps/web/src/lib/library/selection.ts` to satisfy this file and must not
 * edit it; a wrong test comes back to the acceptance lane instead.
 *
 * RED MECHANISM: the module under test does not exist yet, so it is loaded
 * through a variable-specifier dynamic import (a static import would fail
 * `pnpm check` with TS2307 and fail vitest at collection — both are import
 * failures, not assertion RED). Every case gates on `requireModule`, whose
 * `not.toBeNull()` assertion names the case ID: on current code each case
 * fails as an assertion with expected (the locked contract) vs actual
 * (module missing). After the build the same assertions run against the real
 * module with no edits here.
 *
 * LOCKED CONTRACT (`apps/web/src/lib/library/selection.ts`):
 * - A selection is an ordered `string[]` of stable track IDs in checkbox
 *   activation order: no duplicates, no empty/non-string entries. IDs compare
 *   exactly (case-sensitive), matching the keyed list and the snapshot
 *   dedupe discipline.
 * - `createSelection(): string[]` returns a fresh empty selection.
 * - `toggleSelected(selection, id)`: `id` must be a non-empty string or the
 *   call is a no-op returning the same reference. A present ID is removed
 *   (all occurrences, so duplicate inputs self-heal); an absent ID is
 *   appended at the end. Changes return a new array; the input is never
 *   mutated.
 * - `clearSelection(selection)`: returns a fresh `[]`; the input is never
 *   mutated.
 * - `pruneSelection(selection, loadedIds)`: keeps selected IDs present in
 *   `loadedIds` (exact match, duplicates in `loadedIds` ignored), preserving
 *   activation order — never loaded order. Drops non-string/empty entries
 *   from the selection. No-op (nothing dropped, nothing sanitized) returns
 *   the same reference; otherwise a new array. Inputs never mutated.
 * - `hiddenSelectedCount(selection, visibleIds)`: counts valid selected IDs
 *   absent from `visibleIds` (exact match). Invalid entries ignored.
 * - `appendSelection(queue, selection, loaded)`: appends current loaded
 *   metadata for the selection in activation order through the existing
 *   queue dedupe semantics (first loaded occurrence wins; already-queued IDs
 *   are skipped, never duplicated). Missing IDs are ignored — neither added
 *   nor counted as skipped. New tracks carry `{ id, title, artist,
 *   streamUrl: '' }` (the metadata-only contract: `displayTitle` /
 *   `displayArtist(item) ?? ''`, empty stream URL). Existing entries, order
 *   and current track are preserved; an empty queue selects its first added
 *   ID (currentIndex 0) under current queue semantics. Returns
 *   `{ queue, added, skipped }` with `added` newly appended and `skipped`
 *   already-queued duplicates. When `added` is 0 the same queue reference
 *   returns. Duplicate selection inputs append once (first occurrence).
 *   Inputs never mutated.
 * - `selectionCountLabel(count)`: `${count} selected` for a finite integer
 *   `count >= 0`; anything else clamps to `0 selected`.
 * - `appendResultLabel(added, skipped)`: `Added ${added} ${track|tracks}
 *   (${skipped} already queued)` with exact singular/plural.
 * - All helpers are pure: no storage, network, clock or randomness, and no
 *   `window` required.
 *
 * CASE INDEX (stable IDs; SUB-B-* live in tests/library-subset-append.spec.ts):
 * SUB-U-01 create fresh empty, SUB-U-02 activation order, SUB-U-03 deselect,
 * SUB-U-04 reselect to end, SUB-U-05 no duplicates, SUB-U-06 invalid IDs,
 * SUB-U-07 no input mutation, SUB-U-08 clear, SUB-U-09 prune missing,
 * SUB-U-10 prune ignores loaded order, SUB-U-11 prune duplicate loaded IDs,
 * SUB-U-12 hidden count, SUB-U-13 append order + metadata, SUB-U-14 dedupe
 * counts, SUB-U-15 missing ignored, SUB-U-16 queue preserved, SUB-U-17 empty
 * no-op, SUB-U-18 current metadata, SUB-U-19 append purity, SUB-U-20 empty
 * queue first-added + metadata-only URL, SUB-U-21 count label, SUB-U-22
 * result label, SUB-U-23 no globals, SUB-U-24 duplicate selection appends once.
 */

interface AppendResult {
	queue: QueueState;
	added: number;
	skipped: number;
}

interface SelectionModule {
	createSelection: () => string[];
	toggleSelected: (selection: readonly string[], id: unknown) => string[];
	clearSelection: (selection: readonly string[]) => string[];
	pruneSelection: (selection: readonly string[], loadedIds: readonly string[]) => string[];
	hiddenSelectedCount: (selection: readonly string[], visibleIds: readonly string[]) => number;
	appendSelection: (
		queue: QueueState,
		selection: readonly string[],
		loaded: readonly LibraryItem[]
	) => AppendResult;
	selectionCountLabel: (count: number) => string;
	appendResultLabel: (added: number, skipped: number) => string;
}

function isSelectionModule(value: unknown): value is SelectionModule {
	if (value === null || typeof value !== 'object') return false;
	const candidate = value as Record<string, unknown>;
	return (
		typeof candidate['createSelection'] === 'function' &&
		typeof candidate['toggleSelected'] === 'function' &&
		typeof candidate['clearSelection'] === 'function' &&
		typeof candidate['pruneSelection'] === 'function' &&
		typeof candidate['hiddenSelectedCount'] === 'function' &&
		typeof candidate['appendSelection'] === 'function' &&
		typeof candidate['selectionCountLabel'] === 'function' &&
		typeof candidate['appendResultLabel'] === 'function'
	);
}

/**
 * Deliberately typed `string` (not a literal): TypeScript resolves literal
 * dynamic imports at typecheck time, which would fail `pnpm check` while the
 * module does not exist yet. A `string` specifier defers to the runtime,
 * where vitest throws a catchable resolution error on RED.
 */
const MODULE_SPECIFIER: string = './selection.js';

async function loadModule(): Promise<SelectionModule | null> {
	try {
		const loaded: unknown = await import(MODULE_SPECIFIER);
		return isSelectionModule(loaded) ? loaded : null;
	} catch {
		return null;
	}
}

async function requireModule(caseId: string): Promise<SelectionModule> {
	const mod = await loadModule();
	expect(
		mod,
		`${caseId}: selection.ts ships the locked contract (see this file's header)`
	).not.toBeNull();
	if (mod === null) throw new Error(`${caseId}: selection module missing`);
	return mod;
}

const LOADED: readonly LibraryItem[] = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Harbor Lights' },
	{ id: 'row-bravo', title: 'Bravo Song', artist: 'Next Account' }
];

function track(id: string, title: string, artist = ''): Track {
	return { id, title, artist, streamUrl: '' };
}

describe('selection fixtures (harness self-checks, no module required)', () => {
	it('fixture IDs are unique and render through the shared display helpers', () => {
		const ids = LOADED.map((item) => item.id);
		expect(new Set(ids).size).toBe(ids.length);
		expect(displayTitle(LOADED[1]!)).toBe('Alpha Song');
		expect(displayArtist(LOADED[1]!)).toBeNull();
		expect(displayArtist(LOADED[0]!)).toBe('Harbor Lights');
	});

	it('the existing queue dedupe the append contract reuses ignores repeats', () => {
		let queue = createQueue([]);
		queue = addTrack(queue, track('row-mike', 'Mike Song', 'Harbor Lights'));
		const before = queue;
		queue = addTrack(queue, track('row-mike', 'Mike Song', 'Harbor Lights'));
		expect(queue).toBe(before);
		expect(queue.tracks).toHaveLength(1);
		expect(currentTrack(queue)?.id).toBe('row-mike');
	});
});

describe('selection activation order', () => {
	it('SUB-U-01 createSelection returns a fresh empty selection', async () => {
		const mod = await requireModule('SUB-U-01');

		const first = mod.createSelection();
		const second = mod.createSelection();

		expect(first).toEqual([]);
		expect(second).toEqual([]);
		expect(first).not.toBe(second);
	});

	it('SUB-U-02 toggling appends IDs in activation order', async () => {
		const mod = await requireModule('SUB-U-02');

		let selection = mod.createSelection();
		selection = mod.toggleSelected(selection, 'row-mike');
		selection = mod.toggleSelected(selection, 'row-zulu');
		selection = mod.toggleSelected(selection, 'row-alpha');

		expect(selection).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	});

	it('SUB-U-03 deselecting removes the ID and keeps survivor order', async () => {
		const mod = await requireModule('SUB-U-03');

		const selection = mod.toggleSelected(
			mod.toggleSelected(mod.toggleSelected(mod.createSelection(), 'row-mike'), 'row-zulu'),
			'row-alpha'
		);

		expect(mod.toggleSelected(selection, 'row-zulu')).toEqual(['row-mike', 'row-alpha']);
	});

	it('SUB-U-04 reselecting appends the ID at the end', async () => {
		const mod = await requireModule('SUB-U-04');

		let selection = mod.toggleSelected(
			mod.toggleSelected(mod.toggleSelected(mod.createSelection(), 'row-mike'), 'row-zulu'),
			'row-alpha'
		);
		selection = mod.toggleSelected(selection, 'row-zulu');

		expect(mod.toggleSelected(selection, 'row-zulu')).toEqual([
			'row-mike',
			'row-alpha',
			'row-zulu'
		]);
	});

	it('SUB-U-05 toggling never duplicates: reselect removes, duplicates self-heal', async () => {
		const mod = await requireModule('SUB-U-05');

		const selected = mod.toggleSelected(mod.createSelection(), 'row-mike');
		expect(mod.toggleSelected(selected, 'row-mike')).toEqual([]);

		// Defensive: a duplicate input deselects to zero, never to one leftover.
		expect(mod.toggleSelected(['row-mike', 'row-zulu', 'row-mike'], 'row-mike')).toEqual([
			'row-zulu'
		]);
	});

	it.each([[null], [undefined], [42], [true], [{}], [[]], ['']])(
		'SUB-U-06 toggle ignores invalid ID %s as a no-op',
		async (id) => {
			const mod = await requireModule('SUB-U-06');

			const selection = ['row-mike'];
			const out = mod.toggleSelected(selection, id);

			expect(out).toBe(selection);
			expect(selection).toEqual(['row-mike']);
		}
	);

	it('SUB-U-07 toggle, clear and prune never mutate their inputs', async () => {
		const mod = await requireModule('SUB-U-07');

		const selection = ['row-mike', 'row-zulu'];
		const loadedIds = ['row-mike', 'row-zulu', 'row-alpha'];
		const beforeSelection = JSON.stringify(selection);
		const beforeLoaded = JSON.stringify(loadedIds);

		const toggled = mod.toggleSelected(selection, 'row-alpha');
		const cleared = mod.clearSelection(selection);
		const pruned = mod.pruneSelection(selection, loadedIds);

		expect(JSON.stringify(selection)).toBe(beforeSelection);
		expect(JSON.stringify(loadedIds)).toBe(beforeLoaded);
		expect(toggled).not.toBe(selection);
		expect(cleared).toEqual([]);
		expect(pruned).toEqual(['row-mike', 'row-zulu']);
	});

	it('SUB-U-08 clear returns empty and leaves the input untouched', async () => {
		const mod = await requireModule('SUB-U-08');

		const selection = ['row-mike', 'row-zulu'];
		const cleared = mod.clearSelection(selection);

		expect(cleared).toEqual([]);
		expect(cleared).not.toBe(selection);
		expect(selection).toEqual(['row-mike', 'row-zulu']);
		expect(mod.clearSelection([])).toEqual([]);
	});
});

describe('selection prune and hidden count', () => {
	it('SUB-U-09 prune drops IDs absent from the loaded snapshot, keeping order', async () => {
		const mod = await requireModule('SUB-U-09');

		expect(
			mod.pruneSelection(['row-mike', 'row-zulu', 'row-alpha'], ['row-mike', 'row-alpha'])
		).toEqual(['row-mike', 'row-alpha']);
		expect(mod.pruneSelection(['row-gone'], ['row-mike'])).toEqual([]);
	});

	it('SUB-U-10 prune ignores loaded order: sort changes never reorder choices', async () => {
		const mod = await requireModule('SUB-U-10');

		const selection = ['row-bravo', 'row-mike', 'row-alpha'];
		// Same IDs in title-sorted, artist-sorted and reversed loaded order.
		expect(mod.pruneSelection(selection, ['row-alpha', 'row-bravo', 'row-mike'])).toBe(selection);
		expect(mod.pruneSelection(selection, ['row-mike', 'row-alpha', 'row-bravo'])).toBe(selection);
		// Partial survival still follows activation order, not loaded order.
		expect(mod.pruneSelection(selection, ['row-alpha', 'row-bravo'])).toEqual([
			'row-bravo',
			'row-alpha'
		]);
	});

	it('SUB-U-11 prune tolerates duplicate loaded IDs', async () => {
		const mod = await requireModule('SUB-U-11');

		expect(
			mod.pruneSelection(['row-mike', 'row-zulu'], ['row-mike', 'row-mike', 'row-zulu'])
		).toEqual(['row-mike', 'row-zulu']);
	});

	it('SUB-U-12 hiddenSelectedCount counts selected IDs outside the visible set', async () => {
		const mod = await requireModule('SUB-U-12');

		expect(
			mod.hiddenSelectedCount(
				['row-mike', 'row-zulu', 'row-alpha'],
				['row-mike', 'row-alpha']
			)
		).toBe(1);
		expect(mod.hiddenSelectedCount(['row-mike'], ['row-mike'])).toBe(0);
		expect(mod.hiddenSelectedCount([], ['row-mike'])).toBe(0);
		expect(mod.hiddenSelectedCount(['row-mike', 'row-zulu'], [])).toBe(2);
	});
});

describe('selection append through queue dedupe', () => {
	it('SUB-U-13 append adds current loaded metadata in activation order', async () => {
		const mod = await requireModule('SUB-U-13');

		const { queue, added, skipped } = mod.appendSelection(
			createQueue([]),
			['row-mike', 'row-zulu', 'row-alpha'],
			LOADED
		);

		expect(added).toBe(3);
		expect(skipped).toBe(0);
		expect(queue.tracks.map((entry) => entry.id)).toEqual([
			'row-mike',
			'row-zulu',
			'row-alpha'
		]);
		expect(queue.tracks[0]).toMatchObject({
			id: 'row-mike',
			title: 'Mike Song',
			artist: 'Harbor Lights',
			streamUrl: ''
		});
		// Untitled metadata falls back to the shared display contract.
		expect(queue.tracks[2]).toMatchObject({
			id: 'row-alpha',
			title: 'Alpha Song',
			artist: '',
			streamUrl: ''
		});
	});

	it('SUB-U-14 append dedupes already-queued IDs and reports added vs skipped', async () => {
		const mod = await requireModule('SUB-U-14');

		const queued = addTrack(createQueue([]), track('row-zulu', 'Zulu Song', 'Harbor Lights'));
		const { queue, added, skipped } = mod.appendSelection(
			queued,
			['row-mike', 'row-zulu', 'row-alpha'],
			LOADED
		);

		expect(added).toBe(2);
		expect(skipped).toBe(1);
		expect(queue.tracks.map((entry) => entry.id)).toEqual([
			'row-zulu',
			'row-mike',
			'row-alpha'
		]);
	});

	it('SUB-U-15 append ignores missing IDs: neither added nor skipped', async () => {
		const mod = await requireModule('SUB-U-15');

		const queue = createQueue([]);
		const result = mod.appendSelection(queue, ['row-gone', 'row-missing'], LOADED);

		expect(result.added).toBe(0);
		expect(result.skipped).toBe(0);
		expect(result.queue).toBe(queue);
		expect(result.queue.tracks).toEqual([]);

		const mixed = mod.appendSelection(queue, ['row-mike', 'row-gone'], LOADED);
		expect(mixed.added).toBe(1);
		expect(mixed.skipped).toBe(0);
		expect(mixed.queue.tracks.map((entry) => entry.id)).toEqual(['row-mike']);
	});

	it('SUB-U-16 append preserves every existing entry, order and current ID', async () => {
		const mod = await requireModule('SUB-U-16');

		let queue = createQueue([]);
		queue = addTrack(queue, track('row-bravo', 'Bravo Song', 'Next Account'));
		queue = addTrack(queue, track('row-zulu', 'Zulu Song', 'Harbor Lights'));
		const beforeIds = queue.tracks.map((entry) => entry.id);
		const beforeCurrent = currentTrack(queue)?.id;

		const { queue: next, added, skipped } = mod.appendSelection(
			queue,
			['row-mike', 'row-alpha'],
			LOADED
		);

		expect(added).toBe(2);
		expect(skipped).toBe(0);
		expect(next.tracks.map((entry) => entry.id)).toEqual([
			...beforeIds,
			'row-mike',
			'row-alpha'
		]);
		expect(currentTrack(next)?.id).toBe(beforeCurrent);
		expect(next.currentIndex).toBe(queue.currentIndex);
	});

	it('SUB-U-17 append with an empty selection is a queue no-op', async () => {
		const mod = await requireModule('SUB-U-17');

		const queue = addTrack(createQueue([]), track('row-zulu', 'Zulu Song', 'Harbor Lights'));
		const result = mod.appendSelection(queue, [], LOADED);

		expect(result.added).toBe(0);
		expect(result.skipped).toBe(0);
		expect(result.queue).toBe(queue);
	});

	it('SUB-U-18 append uses current loaded metadata, never stale titles', async () => {
		const mod = await requireModule('SUB-U-18');

		const refreshed: readonly LibraryItem[] = [
			{ id: 'row-mike', title: 'Mike Song refreshed', artist: 'Harbor Lights' }
		];
		const { queue } = mod.appendSelection(createQueue([]), ['row-mike'], refreshed);

		expect(queue.tracks[0]).toMatchObject({
			id: 'row-mike',
			title: 'Mike Song refreshed',
			artist: 'Harbor Lights'
		});
	});

	it('SUB-U-19 append never mutates the queue, selection or loaded inputs', async () => {
		const mod = await requireModule('SUB-U-19');

		const queue = addTrack(createQueue([]), track('row-zulu', 'Zulu Song', 'Harbor Lights'));
		const selection = ['row-mike', 'row-alpha'];
		const loaded = [...LOADED];
		const beforeQueue = JSON.stringify(queue);
		const beforeSelection = JSON.stringify(selection);
		const beforeLoaded = JSON.stringify(loaded);

		const result = mod.appendSelection(queue, selection, loaded);

		expect(result.added).toBe(2);
		expect(JSON.stringify(queue)).toBe(beforeQueue);
		expect(JSON.stringify(selection)).toBe(beforeSelection);
		expect(JSON.stringify(loaded)).toBe(beforeLoaded);
		expect(result.queue).not.toBe(queue);
		expect(result.queue.tracks).not.toBe(queue.tracks);
	});

	it('SUB-U-20 an empty queue selects its first added ID with a metadata-only URL', async () => {
		const mod = await requireModule('SUB-U-20');

		const { queue } = mod.appendSelection(createQueue([]), ['row-alpha'], LOADED);

		expect(queue.currentIndex).toBe(0);
		expect(currentTrack(queue)?.id).toBe('row-alpha');
		expect(queue.tracks[0]?.streamUrl).toBe('');
	});

	it('SUB-U-24 a duplicate selection input appends once, first occurrence wins', async () => {
		const mod = await requireModule('SUB-U-24');

		const { queue, added, skipped } = mod.appendSelection(
			createQueue([]),
			['row-mike', 'row-mike', 'row-alpha'],
			LOADED
		);

		expect(added).toBe(2);
		expect(skipped).toBe(0);
		expect(queue.tracks.map((entry) => entry.id)).toEqual(['row-mike', 'row-alpha']);
	});
});

describe('selection labels and purity', () => {
	it('SUB-U-21 selectionCountLabel renders "<n> selected" and clamps invalid counts', async () => {
		const mod = await requireModule('SUB-U-21');

		expect(mod.selectionCountLabel(0)).toBe('0 selected');
		expect(mod.selectionCountLabel(1)).toBe('1 selected');
		expect(mod.selectionCountLabel(2)).toBe('2 selected');
		for (const bad of [
			Number.NaN,
			Number.POSITIVE_INFINITY,
			Number.NEGATIVE_INFINITY,
			-1,
			1.5,
			'2' as unknown as number,
			null as unknown as number,
			undefined as unknown as number
		]) {
			expect(mod.selectionCountLabel(bad), `clamped: ${String(bad)}`).toBe('0 selected');
		}
	});

	it('SUB-U-22 appendResultLabel reports added vs already queued with exact plural', async () => {
		const mod = await requireModule('SUB-U-22');

		expect(mod.appendResultLabel(2, 1)).toBe('Added 2 tracks (1 already queued)');
		expect(mod.appendResultLabel(1, 0)).toBe('Added 1 track (0 already queued)');
		expect(mod.appendResultLabel(0, 2)).toBe('Added 0 tracks (2 already queued)');
	});

	it('SUB-U-23 helpers run with no window, storage or network globals', async () => {
		const mod = await requireModule('SUB-U-23');

		expect(() => mod.createSelection()).not.toThrow();
		expect(() => mod.toggleSelected([], 'row-mike')).not.toThrow();
		expect(() => mod.clearSelection(['row-mike'])).not.toThrow();
		expect(() => mod.pruneSelection(['row-mike'], ['row-mike'])).not.toThrow();
		expect(() => mod.hiddenSelectedCount(['row-mike'], [])).not.toThrow();
		expect(() => mod.appendSelection(createQueue([]), ['row-mike'], LOADED)).not.toThrow();
		expect(() => mod.selectionCountLabel(2)).not.toThrow();
		expect(() => mod.appendResultLabel(2, 1)).not.toThrow();
	});
});
