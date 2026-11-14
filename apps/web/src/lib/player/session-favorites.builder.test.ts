/** Builder-owned session Favorites API tests over real storage helpers and in-memory storage. */
import { describe, expect, it } from 'vitest';
import { createPlayerSession } from './session.js';
import { createQueue, type Track } from './queue.js';
import { LIBRARY_AUTH_CONTEXT_KEY, LIBRARY_FAVORITES_KEY, type SnapshotStorage } from '../library/cache.js';

const mk = (id: string, title = `Track ${id.toUpperCase()}`): Track => ({ id, title, artist: 'Artist', streamUrl: `/${id}` });
const A = mk('a'), B = mk('b'), C = mk('c');
const REPEAT_KEY = 'nntune:library-repeat:v1';
const keyFor = (context: string) => `${LIBRARY_FAVORITES_KEY}.${context}`;

function makeStorage(failWrites = { value: false }) {
	const values = new Map<string, string>();
	const storage: SnapshotStorage = {
		getItem: (key) => values.get(key) ?? null,
		setItem: (key, value) => {
			if (failWrites.value) throw new Error('quota');
			values.set(key, value);
		},
		removeItem: (key) => { values.delete(key); }
	};
	return { storage, values, failWrites };
}

function setup(context: string | null = 'ctx-1', tracks: readonly Track[] = [A, B, C]) {
	const sessionStore = makeStorage();
	const local = makeStorage();
	if (context) sessionStore.values.set(LIBRARY_AUTH_CONTEXT_KEY, context);
	const session = createPlayerSession({ flush: async () => {} });
	session.setQueue({ ...createQueue(tracks), currentIndex: 1 });
	session.start(local.storage, sessionStore.storage);
	return { session, sessionStore, local };
}
const favs = (s: ReturnType<typeof setup>['session']) => s.snapshot().favorites;
const stored = (local: ReturnType<typeof makeStorage>, context = 'ctx-1') => {
	const raw = local.values.get(keyFor(context));
	return raw === undefined ? undefined : JSON.parse(raw);
};

describe('session favorites', () => {
	it('bootstraps from scoped storage once after start', () => {
		const { session, sessionStore, local } = setup();
		local.values.set(keyFor('ctx-1'), JSON.stringify(['a', 'x', 'a', '']));
		session.initializeFavorites();
		expect(favs(session)).toEqual(['a', 'x']);
		// A second initialize is ignored even with different storage content.
		local.values.set(keyFor('ctx-1'), JSON.stringify(['c']));
		session.initializeFavorites();
		expect(favs(session)).toEqual(['a', 'x']);
	});

	it('keeps memory authoritative after a failed write and retries the complete list', () => {
		const { session, sessionStore, local } = setup();
		local.values.set(keyFor('ctx-1'), JSON.stringify(['a']));
		session.initializeFavorites();
		local.failWrites.value = true;
		expect(session.toggleFavorite('b')).toBe(true);
		expect(favs(session)).toEqual(['a', 'b']);
		expect(stored(local)).toEqual(['a']); // stale storage
		session.initializeFavorites();
		expect(favs(session)).toEqual(['a', 'b']);
		local.failWrites.value = false;
		expect(session.toggleFavorite('c')).toBe(true);
		expect(favs(session)).toEqual(['a', 'b', 'c']);
		expect(stored(local)).toEqual(['a', 'b', 'c']);
	});

	it('preserves hidden ids and supports arrangement, set, remove and clear', () => {
		const { session, sessionStore, local } = setup();
		local.values.set(keyFor('ctx-1'), JSON.stringify(['hidden-1', 'a']));
		session.initializeFavorites();
		session.toggleFavorite('b');
		expect(favs(session)).toEqual(['hidden-1', 'a', 'b']);
		expect(session.toggleFavorite('a')).toBe(true);
		expect(favs(session)).toEqual(['hidden-1', 'b']);
		expect(session.setFavorites(['b', 'hidden-1', 'hidden-2'])).toBe(true);
		expect(favs(session)).toEqual(['b', 'hidden-1', 'hidden-2']);
		expect(stored(local)).toEqual(['b', 'hidden-1', 'hidden-2']);
		session.toggleFavorite('hidden-1');
		expect(stored(local)).toEqual(['b', 'hidden-2']);
		expect(session.setFavorites([])).toBe(true);
		expect(favs(session)).toEqual([]);
		expect(stored(local)).toEqual([]);
	});

	it('toggleQueuedFavorite gives exact feedback and ignores absent ids', () => {
		const { session, sessionStore, local } = setup();
		session.initializeFavorites();
		expect(session.toggleQueuedFavorite('b')).toBe(true);
		expect(favs(session)).toEqual(['b']);
		expect(session.snapshot().queueFeedback).toBe('Starred Track B');
		expect(session.toggleQueuedFavorite('b')).toBe(true);
		expect(favs(session)).toEqual([]);
		expect(session.snapshot().queueFeedback).toBe('Unstarred Track B');
		const before = session.snapshot();
		expect(session.toggleQueuedFavorite('missing')).toBe(false);
		expect(session.snapshot()).toBe(before);
		expect(stored(local)).toEqual([]);
	});

	it('notifies mounted route aftermath only for accepted queued star changes', () => {
		const { session } = setup();
		const outcomes: [string, string[]][] = [];
		const off = session.onQueuedFavorite((id) => outcomes.push([id, [...favs(session)]]));
		expect(session.toggleQueuedFavorite('a')).toBe(false);
		session.initializeFavorites();
		expect(session.toggleQueuedFavorite('missing')).toBe(false);
		expect(outcomes).toEqual([]);
		session.toggleQueuedFavorite('a');
		session.toggleQueuedFavorite('a');
		expect(outcomes).toEqual([['a', ['a']], ['a', []]]);
		off();
		session.toggleQueuedFavorite('b');
		expect(outcomes).toHaveLength(2);
	});

	it('drops queued star observers on layout disposal, not on auth reset', () => {
		const { session } = setup();
		let count = 0;
		session.onQueuedFavorite(() => { count++; });
		session.initializeFavorites();
		session.toggleQueuedFavorite('a');
		session.resetAtAuthBoundary();
		session.initializeFavorites();
		session.setQueue(createQueue([A]));
		session.toggleQueuedFavorite('a');
		expect(count).toBe(2);
		session.destroy();
		session.start(null);
		session.initializeFavorites();
		session.setQueue(createQueue([A]));
		session.toggleQueuedFavorite('a');
		expect(count).toBe(2);
	});

	it('never changes the queue or current selection', () => {
		const { session, sessionStore, local } = setup();
		session.initializeFavorites();
		const queue = session.snapshot().queue;
		session.toggleQueuedFavorite('a');
		session.toggleFavorite('c');
		session.setFavorites(['b']);
		expect(session.snapshot().queue).toBe(queue);
		expect(session.snapshot().queue.currentIndex).toBe(1);
		expect(session.snapshot().queue.tracks.map((t) => t.id)).toEqual(['a', 'b', 'c']);
	});

	it('does nothing before initialize', () => {
		const { session, local } = setup();
		const before = session.snapshot();
		expect(session.setFavorites(['a'])).toBe(false);
		expect(session.toggleFavorite('a')).toBe(false);
		expect(session.toggleQueuedFavorite('a')).toBe(false);
		expect(session.snapshot()).toBe(before);
		expect(local.values.size).toBe(0);
	});

	it('does nothing before start', () => {
		const sessionStore = makeStorage();
		const local = makeStorage();
		sessionStore.values.set(LIBRARY_AUTH_CONTEXT_KEY, 'ctx-1');
		const session = createPlayerSession({ flush: async () => {} });
		session.initializeFavorites();
		expect(session.setFavorites(['a'])).toBe(false);
		expect(favs(session)).toEqual([]);
		expect(local.values.size).toBe(0);
	});

	it('purges stored favorites before the first authenticated catalogue bootstrap', () => {
		const { session, sessionStore, local } = setup();
		local.values.set(keyFor('ctx-1'), JSON.stringify(['a']));
		local.values.set(LIBRARY_FAVORITES_KEY, JSON.stringify(['legacy']));
		local.values.set(keyFor('ctx-other'), JSON.stringify(['z']));
		expect(favs(session)).toEqual([]);
		session.resetAtAuthBoundary();
		expect(local.values.has(keyFor('ctx-1'))).toBe(false);
		expect(local.values.has(LIBRARY_FAVORITES_KEY)).toBe(false);
		expect(stored(local, 'ctx-other')).toEqual(['z']);
		sessionStore.values.delete(LIBRARY_AUTH_CONTEXT_KEY);
		sessionStore.values.set(LIBRARY_AUTH_CONTEXT_KEY, 'ctx-new');
		session.initializeFavorites();
		expect(favs(session)).toEqual([]);
	});

	it('auth reset deletes old scoped and legacy keys and blocks changes until re-initialize', () => {
		const { session, sessionStore, local } = setup();
		local.values.set(keyFor('ctx-1'), JSON.stringify(['a']));
		local.values.set(LIBRARY_FAVORITES_KEY, JSON.stringify(['legacy']));
		local.values.set(keyFor('ctx-other'), JSON.stringify(['z']));
		session.initializeFavorites();
		session.resetAtAuthBoundary();
		expect(favs(session)).toEqual([]);
		expect(local.values.has(keyFor('ctx-1'))).toBe(false);
		expect(local.values.has(LIBRARY_FAVORITES_KEY)).toBe(false);
		expect(local.values.has(keyFor('ctx-other'))).toBe(true);
		const before = session.snapshot();
		expect(session.setFavorites(['b'])).toBe(false);
		expect(session.toggleFavorite('b')).toBe(false);
		expect(session.snapshot()).toBe(before);
		expect(local.values.has(keyFor('ctx-1'))).toBe(false);
	});

	it('fresh account bootstrap after reset does not leak the previous account', () => {
		const { session, sessionStore, local } = setup();
		local.values.set(keyFor('ctx-1'), JSON.stringify(['a']));
		session.initializeFavorites();
		session.resetAtAuthBoundary();
		sessionStore.values.set(LIBRARY_AUTH_CONTEXT_KEY, 'ctx-2');
		session.initializeFavorites();
		expect(favs(session)).toEqual([]);
		session.toggleFavorite('b');
		expect(stored(local, 'ctx-2')).toEqual(['b']);
		expect(local.values.has(keyFor('ctx-1'))).toBe(false);
	});

	it('isolates separate sessions', () => {
		const one = setup('ctx-1');
		const two = setup('ctx-2');
		one.session.initializeFavorites();
		two.session.initializeFavorites();
		one.session.toggleFavorite('a');
		expect(favs(one.session)).toEqual(['a']);
		expect(favs(two.session)).toEqual([]);
		expect(two.local.values.size).toBe(0);
		one.session.resetAtAuthBoundary();
		two.session.toggleFavorite('b');
		expect(favs(two.session)).toEqual(['b']);
	});

	it('destroy clears memory, keeps persisted data and preferences, and blocks changes', () => {
		const { session, sessionStore, local } = setup();
		local.values.set(REPEAT_KEY, 'queue');
		local.values.set(keyFor('ctx-1'), JSON.stringify(['a']));
		session.initializeFavorites();
		session.destroy();
		expect(favs(session)).toEqual([]);
		expect(session.setFavorites(['b'])).toBe(false);
		expect(session.toggleFavorite('b')).toBe(false);
		expect(session.toggleQueuedFavorite('a')).toBe(false);
		expect(favs(session)).toEqual([]);
		expect(stored(local)).toEqual(['a']);
		expect(local.values.get(REPEAT_KEY)).toBe('queue');
	});
});

describe('Favorites synchronous retirement fences', () => {
	it.each(['auth', 'destroy', 'favorites', 'queue'] as const)('does not persist or report success after a %s subscriber supersedes publication', (boundary) => {
		const { session, local } = setup();
		session.initializeFavorites();
		let armed = false;
		const stop = session.subscribe((snapshot) => {
			if (!armed || !snapshot.favorites.includes('a')) return;
			armed = false;
			if (boundary === 'auth') session.resetAtAuthBoundary();
			else if (boundary === 'destroy') session.destroy();
			else if (boundary === 'favorites') session.setFavorites(['c']);
			else session.setQueue(createQueue([C]));
		});
		armed = true;
		expect(session.toggleQueuedFavorite('a')).toBe(false);
		expect(session.snapshot().queueFeedback).toBe('');
		if (boundary === 'favorites') {
			expect(favs(session)).toEqual(['c']);
			expect(stored(local)).toEqual(['c']);
		} else {
			expect(stored(local)).toBeUndefined();
			if (boundary === 'queue') expect(session.snapshot().queue).toEqual(createQueue([C]));
			else expect(favs(session)).toEqual([]);
		}
		stop();
		session.destroy();
	});

	it('a scoped-key read that resets auth cannot write the outgoing Favorites into either context', () => {
		const { session, sessionStore, local } = setup();
		session.initializeFavorites();
		const read = sessionStore.storage.getItem;
		let armed = true;
		sessionStore.storage.getItem = (key) => {
			const value = read(key);
			if (armed && key === LIBRARY_AUTH_CONTEXT_KEY) {
				armed = false;
				session.resetAtAuthBoundary();
				sessionStore.values.set(LIBRARY_AUTH_CONTEXT_KEY, 'ctx-2');
			}
			return value;
		};
		expect(session.toggleQueuedFavorite('a')).toBe(false);
		expect(stored(local)).toBeUndefined();
		expect(stored(local, 'ctx-2')).toBeUndefined();
		expect(favs(session)).toEqual([]);
		expect(session.snapshot().queueFeedback).toBe('');
		session.destroy();
	});

	it('an old bootstrap read cannot republish Favorites after auth reset', () => {
		const { session, local } = setup();
		local.values.set(keyFor('ctx-1'), JSON.stringify(['a']));
		const read = local.storage.getItem;
		let armed = true;
		local.storage.getItem = (key) => {
			const value = read(key);
			if (armed && key === keyFor('ctx-1')) {
				armed = false;
				session.resetAtAuthBoundary();
			}
			return value;
		};
		session.initializeFavorites();
		expect(favs(session)).toEqual([]);
		expect(session.setFavorites(['b'])).toBe(false);
		expect(stored(local)).toBeUndefined();
		session.destroy();
	});

	it('retirement during queue feedback suppresses all queued-Favorites route aftermath', () => {
		const { session } = setup();
		session.initializeFavorites();
		const aftermath: string[] = [];
		session.onQueuedFavorite((id) => { aftermath.push(id); });
		let armed = false;
		const stop = session.subscribe((snapshot) => {
			if (!armed || snapshot.queueFeedback !== 'Starred Track A') return;
			armed = false;
			session.resetAtAuthBoundary();
		});
		armed = true;
		expect(session.toggleQueuedFavorite('a')).toBe(false);
		expect(aftermath).toEqual([]);
		expect(session.snapshot().queueFeedback).toBe('');
		stop();
		session.destroy();
	});

	it('the first retiring aftermath listener fences the remaining old-action listeners', () => {
		const { session } = setup();
		session.initializeFavorites();
		const aftermath: string[] = [];
		session.onQueuedFavorite(() => { aftermath.push('first'); session.resetAtAuthBoundary(); });
		session.onQueuedFavorite(() => { aftermath.push('stale'); });
		expect(session.toggleQueuedFavorite('a')).toBe(false);
		expect(aftermath).toEqual(['first']);
		expect(favs(session)).toEqual([]);
		expect(session.snapshot().queueFeedback).toBe('');
		session.destroy();
	});

	it('an old bootstrap read cannot overwrite a newer synchronous Favorites action', () => {
		const { session, local } = setup();
		local.values.set(keyFor('ctx-1'), JSON.stringify(['a']));
		const read = local.storage.getItem;
		let armed = true;
		local.storage.getItem = (key) => {
			const value = read(key);
			if (armed && key === keyFor('ctx-1')) {
				armed = false;
				session.setFavorites(['c']);
			}
			return value;
		};
		session.initializeFavorites();
		expect(favs(session)).toEqual(['c']);
		expect(stored(local)).toEqual(['c']);
		session.destroy();
	});
});

describe('incoming Favorites action intent', () => {
	it.each([false, true])('add-only and toggle remain distinct with refused persistence=%s', (refused) => {
		const { session, local } = setup();
		session.initializeFavorites();
		session.toggleFavorite('a');
		local.failWrites.value = refused;
		expect(session.favoriteQueuedIds(['a', 'b'], 'add')).toBe(true);
		expect(favs(session)).toEqual(['a', 'b']);
		expect(session.snapshot().queueFeedback).toBe('Starred 1 track; 1 was already starred.');
		expect(stored(local)).toEqual(refused ? ['a'] : ['a', 'b']);
		expect(session.favoriteQueuedIds(['a', 'b'], 'add')).toBe(true);
		expect(favs(session)).toEqual(['a', 'b']);
		expect(session.favoriteQueuedIds(['a', 'b'])).toBe(true);
		expect(favs(session)).toEqual([]);
		expect(session.snapshot().queueFeedback).toBe('Removed 2 tracks from favorites.');
		expect(stored(local)).toEqual(refused ? ['a'] : []);
		session.destroy();
	});
});
