import { afterEach, describe, expect, it, vi } from 'vitest';

import {
	broadcastAuthTransition,
	browserStorage,
	clearSnapshot,
	decodeSnapshot,
	encodeSnapshot,
	evictToCap,
	invalidateLibraryCache,
	loadSnapshot,
	MAX_SNAPSHOT_BYTES,
	MAX_SNAPSHOT_ITEMS,
	saveSnapshot,
	snapshotByteSize,
	staleLabel,
	toLibraryItems,
	LIBRARY_AUTH_CONTEXT_KEY,
	LIBRARY_AUTH_TRANSITION_KEY,
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

describe('toLibraryItems', () => {
	it('accepts a bare array', () => {
		expect(toLibraryItems([item('a'), item('b')])?.map((i) => i.id)).toEqual(['a', 'b']);
	});

	it('accepts an { items } envelope', () => {
		expect(toLibraryItems({ items: [item('a')] })?.map((i) => i.id)).toEqual(['a']);
	});

	it('drops rows without a string id instead of blanking the list', () => {
		expect(
			toLibraryItems([item('a'), { nope: 1 }, { id: 42 }, item('b')])?.map((i) => i.id)
		).toEqual(['a', 'b']);
	});

	it('distinguishes valid empty lists from foreign response shapes', () => {
		expect(toLibraryItems([])).toEqual([]);
		expect(toLibraryItems({ items: [] })).toEqual([]);
		expect(toLibraryItems(null)).toBeNull();
		expect(toLibraryItems({})).toBeNull();
		expect(toLibraryItems({ error: 'not authorized' })).toBeNull();
		expect(toLibraryItems([{ track_id: 'foreign-row' }])).toBeNull();
		expect(toLibraryItems([item('a'), { track_id: 'ignored-row' }])).toEqual([item('a')]);
		expect(toLibraryItems('nope')).toBeNull();
	});
});

describe('evictToCap', () => {
	it('keeps small lists untouched', () => {
		const items = [item('a'), item('b')];
		expect(evictToCap(items)).toEqual(items);
	});

	it('enforces the count cap keeping the head', () => {
		const items = Array.from({ length: MAX_SNAPSHOT_ITEMS + 50 }, (_, i) => item(`t${i}`));
		const kept = evictToCap(items);
		expect(kept).toHaveLength(MAX_SNAPSHOT_ITEMS);
		expect(kept[0]?.id).toBe('t0');
		expect(kept[MAX_SNAPSHOT_ITEMS - 1]?.id).toBe(`t${MAX_SNAPSHOT_ITEMS - 1}`);
	});

	it('enforces the byte cap by dropping the tail', () => {
		const big = Array.from({ length: 20 }, (_, i) =>
			item(`t${i}`, { blob: 'x'.repeat(20_000) })
		);
		const kept = evictToCap(big, 10_000, 30_000);
		expect(kept.length).toBeGreaterThan(0);
		expect(snapshotByteSize(kept, new Date().toISOString())).toBeLessThanOrEqual(30_000);
		// Head preserved: the first item survives eviction.
		expect(kept[0]?.id).toBe('t0');
	});

	it('never exceeds the default byte cap for many small items', () => {
		const items = Array.from({ length: 5_000 }, (_, i) => item(`t${i}`));
		const kept = evictToCap(items);
		expect(kept.length).toBeLessThanOrEqual(MAX_SNAPSHOT_ITEMS);
		expect(snapshotByteSize(kept, new Date().toISOString())).toBeLessThanOrEqual(
			MAX_SNAPSHOT_BYTES
		);
	});

	it('drops a single item that exceeds the byte cap', () => {
		const kept = evictToCap([item('oversized', { blob: 'x'.repeat(250_000) })]);
		expect(kept).toEqual([]);
		expect(snapshotByteSize(kept, new Date().toISOString())).toBeLessThanOrEqual(
			MAX_SNAPSHOT_BYTES
		);
	});
});

describe('snapshot cap literals', () => {
	// Literal pins for the two snapshot caps (`cache.ts:51,54`). Eviction
	// behavior is pinned symbolically elsewhere in this file; these tests
	// pin the exact bound values so a silent cap change turns red.
	//
	// Sensitivity (implementation untouched):
	// - changing MAX_SNAPSHOT_ITEMS off 200 fails `pins the item-count cap at 200`
	//   and `keeps the head 200 by default over 250 small items`;
	// - changing MAX_SNAPSHOT_BYTES off 204_800 fails `pins the byte cap at 204_800`.
	it('pins the item-count cap at 200', () => {
		expect(MAX_SNAPSHOT_ITEMS).toBe(200);
	});

	it('pins the byte cap at 204_800', () => {
		expect(MAX_SNAPSHOT_BYTES).toBe(204_800);
	});

	it('keeps the head 200 by default over 250 small items', () => {
		const items = Array.from({ length: 250 }, (_, i) => item(`t${i}`));
		const kept = evictToCap(items);
		expect(kept).toHaveLength(200);
		expect(kept[0]?.id).toBe('t0');
		expect(kept[199]?.id).toBe('t199');
		expect(snapshotByteSize(kept, new Date().toISOString())).toBeLessThanOrEqual(
			MAX_SNAPSHOT_BYTES
		);
	});
});

describe('snapshot round-trip', () => {
	it('saves and loads a fresh snapshot', () => {
		const storage = memoryStorage();
		const saved = saveSnapshot(storage, [item('a'), item('b')], Date.parse('2026-10-15T12:00:00Z'));
		expect(saved.savedAt).toBe('2026-10-15T12:00:00.000Z');
		expect(loadSnapshot(storage)).toEqual(saved);
	});

	it('fresh success replaces the previous snapshot', () => {
		const storage = memoryStorage();
		saveSnapshot(storage, [item('old')], 1_000);
		const fresh = saveSnapshot(storage, [item('new')], 2_000);
		expect(loadSnapshot(storage)).toEqual(fresh);
		expect(loadSnapshot(storage)?.items.map((i) => i.id)).toEqual(['new']);
	});

	it('scopes snapshots to an auth context and clears the prior account on transition', () => {
		const storage = memoryStorage();
		saveSnapshot(storage, [item('account-a')], 1_000);
		const accountAContext = storage.getItem(LIBRARY_AUTH_CONTEXT_KEY);
		expect(accountAContext).toBeTruthy();
		expect(storage.getItem(`${LIBRARY_SNAPSHOT_KEY}.${accountAContext}`)).not.toBeNull();

		clearSnapshot(storage);
		expect(loadSnapshot(storage)).toBeNull();
		expect(storage.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();

		saveSnapshot(storage, [item('account-b')], 2_000);
		const accountBContext = storage.getItem(LIBRARY_AUTH_CONTEXT_KEY);
		expect(accountBContext).not.toBe(accountAContext);
		expect(loadSnapshot(storage)?.items.map((i) => i.id)).toEqual(['account-b']);
	});

	it('caps on save so storage can never grow unbounded', () => {
		const storage = memoryStorage();
		const items = Array.from({ length: MAX_SNAPSHOT_ITEMS + 10 }, (_, i) => item(`t${i}`));
		const saved = saveSnapshot(storage, items);
		expect(saved.items).toHaveLength(MAX_SNAPSHOT_ITEMS);
		expect(loadSnapshot(storage)?.items).toHaveLength(MAX_SNAPSHOT_ITEMS);
	});

	it('returns null for missing, corrupt, or foreign payloads', () => {
		expect(loadSnapshot(memoryStorage())).toBeNull();
		expect(
			loadSnapshot(memoryStorage({ [LIBRARY_SNAPSHOT_KEY]: 'not-json{' }))
		).toBeNull();
		expect(
			loadSnapshot(memoryStorage({ [LIBRARY_SNAPSHOT_KEY]: JSON.stringify({ nope: 1 }) }))
		).toBeNull();
		expect(decodeSnapshot(JSON.stringify({ savedAt: 'not-a-date', items: [] }))).toBeNull();
	});

	it('tolerates a throwing storage backend', () => {
		const failing: SnapshotStorage = {
			getItem: () => {
				throw new Error('denied');
			},
			setItem: () => {
				throw new Error('denied');
			},
			removeItem: () => {
				throw new Error('denied');
			}
		};
		expect(loadSnapshot(failing)).toBeNull();
		expect(() => saveSnapshot(failing, [item('a')])).not.toThrow();
	});
});

describe('encodeSnapshot', () => {
	it('stamps the save time as ISO', () => {
		const snapshot = encodeSnapshot([item('a')], Date.parse('2026-10-15T12:00:00Z'));
		expect(snapshot.savedAt).toBe('2026-10-15T12:00:00.000Z');
	});
});

describe('staleLabel', () => {
	it('always carries the stale prefix journey specs pin', () => {
		const at = Date.parse('2026-10-15T12:00:00Z');
		expect(staleLabel('2026-10-15T11:59:30Z', at)).toMatch(/^stale — updated /);
		expect(staleLabel('2026-10-15T11:00:00Z', at)).toMatch(/^stale — updated /);
		expect(staleLabel('2026-10-10T11:00:00Z', at)).toMatch(/^stale — updated /);
		expect(staleLabel('garbage', at)).toMatch(/^stale — updated /);
	});

	it('mentions recency for recent snapshots', () => {
		const at = Date.parse('2026-10-15T12:00:00Z');
		expect(staleLabel(new Date(at - 10_000).toISOString(), at)).toContain('just now');
		expect(staleLabel(new Date(at - 5 * 60_000).toISOString(), at)).toContain('5m ago');
	});
});

/** A `window` whose storage getters throw, like a blocked or sandboxed origin. */
function blockedWindow(): Record<string, unknown> {
	const win: Record<string, unknown> = {};
	for (const name of ['sessionStorage', 'localStorage']) {
		Object.defineProperty(win, name, {
			get() {
				throw new DOMException('The operation is insecure.', 'SecurityError');
			}
		});
	}
	return win;
}

describe('storage access that itself throws', () => {
	afterEach(() => {
		vi.unstubAllGlobals();
	});

	it('browserStorage resolves null instead of throwing', () => {
		vi.stubGlobal('window', blockedWindow());
		expect(browserStorage('session')).toBeNull();
		expect(browserStorage('local')).toBeNull();
	});

	it('browserStorage resolves null when there is no window at all', () => {
		expect(browserStorage('session')).toBeNull();
	});

	it('every helper tolerates a null storage', () => {
		expect(loadSnapshot(null)).toBeNull();
		expect(() => saveSnapshot(null, [item('a')])).not.toThrow();
		expect(saveSnapshot(null, [item('a')]).items.map((i) => i.id)).toEqual(['a']);
		expect(() => clearSnapshot(null)).not.toThrow();
		expect(() => broadcastAuthTransition(null)).not.toThrow();
	});

	it('invalidateLibraryCache never throws, so a finished sign-in cannot be undone by cleanup', () => {
		vi.stubGlobal('window', blockedWindow());
		expect(() => invalidateLibraryCache()).not.toThrow();
	});

	it('invalidateLibraryCache clears both areas and broadcasts when storage works', () => {
		const session = memoryStorage();
		saveSnapshot(session, [item('account-a')], 1_000);
		const local = memoryStorage({ [LIBRARY_SNAPSHOT_KEY]: 'legacy' });
		const broadcasts: string[] = [];
		const watchedLocal: SnapshotStorage = {
			getItem: (key) => local.getItem(key),
			setItem: (key, value) => {
				if (key === LIBRARY_AUTH_TRANSITION_KEY) broadcasts.push(value);
				local.setItem(key, value);
			},
			removeItem: (key) => local.removeItem(key)
		};
		vi.stubGlobal('window', { sessionStorage: session, localStorage: watchedLocal });

		invalidateLibraryCache();

		expect(loadSnapshot(session)).toBeNull();
		expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();
		expect(local.getItem(LIBRARY_SNAPSHOT_KEY)).toBeNull();
		expect(broadcasts).toHaveLength(1);
	});
});
