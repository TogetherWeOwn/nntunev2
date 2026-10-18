import { describe, expect, it } from 'vitest';

import {
	decodeSnapshot,
	loadSnapshot,
	MAX_SNAPSHOT_BYTES,
	MAX_SNAPSHOT_ITEMS,
	snapshotByteSize,
	toLibraryItems,
	LIBRARY_AUTH_CONTEXT_KEY,
	LIBRARY_SNAPSHOT_KEY,
	type LibraryItem,
	type SnapshotStorage
} from './cache.js';

function item(id: string, extra: Record<string, unknown> = {}): LibraryItem {
	return { id, title: `Track ${id}`, ...extra };
}

function memoryStorage(seed: Record<string, string> = {}): SnapshotStorage {
	const map = new Map(Object.entries(seed));
	return {
		getItem: (key) => map.get(key) ?? null,
		setItem: (key, value) => {
			map.set(key, value);
		},
		removeItem: (key) => {
			map.delete(key);
		}
	};
}

function snapshotWith(items: unknown[]): string {
	return JSON.stringify({ savedAt: '2026-10-15T12:00:00.000Z', items });
}

describe('toLibraryItems keeps duplicate library IDs out of keyed rendering', () => {
	it('deduplicates a bare-array payload, keeping the first occurrence and original order', () => {
		const result = toLibraryItems([item('b'), item('a'), item('b'), item('c'), item('a')]);
		expect(result?.map((i) => i.id)).toEqual(['b', 'a', 'c']);
	});

	it('deduplicates an { items } envelope payload the same way', () => {
		const result = toLibraryItems({ items: [item('a'), item('b'), item('a'), item('b')] });
		expect(result?.map((i) => i.id)).toEqual(['a', 'b']);
	});

	it('does not merge data from a conflicting duplicate row into the first occurrence', () => {
		const first = { id: 'a', title: 'First Title', artist: 'First Artist' };
		const conflicting = { id: 'a', title: 'Other Title', artist: 'Other Artist', extra: 1 };
		expect(toLibraryItems([first, conflicting])).toEqual([first]);
	});

	it('keeps the first valid occurrence when invalid rows are interleaved', () => {
		const result = toLibraryItems([
			{ id: '' },
			{ title: 'no id at all' },
			item('a'),
			{ id: 42 },
			item('a', { title: 'Duplicate A' }),
			item('b')
		]);
		expect(result).toEqual([item('a'), item('b')]);
	});

	it('collapses a payload of only duplicates of one id to that single row, not null', () => {
		expect(toLibraryItems([item('a'), item('a'), item('a')])).toEqual([item('a')]);
	});

	it('treats ids differing only by case as distinct keys', () => {
		expect(toLibraryItems([item('Track-A'), item('track-a'), item('Track-A')])?.map((i) => i.id)).toEqual([
			'Track-A',
			'track-a'
		]);
	});

	it('leaves an already-unique payload untouched (positive control)', () => {
		const unique = [item('a'), item('b'), item('c')];
		expect(toLibraryItems(unique)).toEqual(unique);
	});

	it('preserves valid-empty versus malformed-payload behavior', () => {
		expect(toLibraryItems([])).toEqual([]);
		expect(toLibraryItems({ items: [] })).toEqual([]);
		expect(toLibraryItems(null)).toBeNull();
		expect(toLibraryItems([{ id: '' }, { id: '' }, { track_id: 'foreign' }])).toBeNull();
	});
});

describe('restored snapshots keep duplicate library IDs out of keyed rendering', () => {
	it('deduplicates snapshot rows, keeping the first occurrence and original order', () => {
		const decoded = decodeSnapshot(snapshotWith([item('b'), item('a'), item('b'), item('a'), item('c')]));
		expect(decoded?.items.map((i) => i.id)).toEqual(['b', 'a', 'c']);
	});

	it('does not merge data from a conflicting duplicate snapshot row', () => {
		const first = { id: 'a', title: 'First Title' };
		const decoded = decodeSnapshot(snapshotWith([first, { id: 'a', title: 'Other Title', extra: 1 }]));
		expect(decoded?.items).toEqual([first]);
	});

	it('deduplicates through loadSnapshot with the auth-scoped storage key', () => {
		const context = 'duplicate-test-context';
		const storage = memoryStorage({
			[LIBRARY_AUTH_CONTEXT_KEY]: context,
			[`${LIBRARY_SNAPSHOT_KEY}.${context}`]: snapshotWith([item('a'), item('a'), item('b')])
		});
		expect(loadSnapshot(storage)?.items.map((i) => i.id)).toEqual(['a', 'b']);
	});

	it('deduplicates before the count cap so unique rows past the cap position survive', () => {
		// 101 copies of 'dup' followed by 100 unique rows: 201 raw rows, 101 unique.
		// Deduplicating first keeps all 101 unique rows; capping to
		// MAX_SNAPSHOT_ITEMS (200) first would drop the last unique row.
		const rows = [
			...Array.from({ length: 101 }, () => item('dup')),
			...Array.from({ length: 100 }, (_, i) => item(`t${i}`))
		];
		const decoded = decodeSnapshot(snapshotWith(rows));
		expect(decoded?.items).toHaveLength(101);
		expect(decoded?.items[0]?.id).toBe('dup');
		expect(decoded?.items.at(-1)?.id).toBe('t99');
		expect(new Set(decoded?.items.map((i) => i.id)).size).toBe(decoded?.items.length);
	});

	it('still enforces the count cap on unique rows', () => {
		const rows = Array.from({ length: MAX_SNAPSHOT_ITEMS + 50 }, (_, i) => item(`t${i}`));
		// Duplicate the head row too: the cap applies after it collapses away.
		const decoded = decodeSnapshot(snapshotWith([item('t0'), ...rows]));
		expect(decoded?.items).toHaveLength(MAX_SNAPSHOT_ITEMS);
		expect(decoded?.items[0]?.id).toBe('t0');
		expect(decoded?.items.at(-1)?.id).toBe(`t${MAX_SNAPSHOT_ITEMS - 1}`);
	});

	it('deduplicates before the byte cap so a repeated oversized row is only counted once', () => {
		// One 120kB row fits the byte cap; the same row twice plus a small row does not.
		// Deduplicating first keeps [big, small]; capping first would drop small.
		const big = item('big', { blob: 'x'.repeat(120_000) });
		const decoded = decodeSnapshot(snapshotWith([big, { ...big }, item('small')]));
		expect(decoded?.items.map((i) => i.id)).toEqual(['big', 'small']);
		expect(snapshotByteSize(decoded?.items ?? [], decoded?.savedAt ?? '')).toBeLessThanOrEqual(
			MAX_SNAPSHOT_BYTES
		);
	});

	it('preserves valid-empty versus malformed snapshot behavior', () => {
		expect(decodeSnapshot(snapshotWith([]))?.items).toEqual([]);
		expect(decodeSnapshot(snapshotWith([{ id: '' }, { id: '' }]))).toBeNull();
		expect(decodeSnapshot('not-json{')).toBeNull();
		expect(decodeSnapshot(null)).toBeNull();
	});
});
