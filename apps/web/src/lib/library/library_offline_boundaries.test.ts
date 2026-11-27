import { afterEach, expect, it, vi } from 'vitest';
import type { ApiClient } from '../api/client.js';
import { AUTH_GENERATION_KEY, SESSION_BINDING_HEADER } from '../auth-fence.js';
import {
	clearSnapshot, decodeSnapshot, establishVerifiedLibraryScope, LIBRARY_AUTH_CONTEXT_KEY,
	LIBRARY_QUEUE_KEY, LIBRARY_SESSION_BINDING_KEY, LIBRARY_SNAPSHOT_KEY, libraryScopeBinding,
	toLibraryItems, type SnapshotStorage
} from './cache.js';
import { displayCollections, loadCollections, type CollectionFilter, type CollectionSort } from './collections.js';
import { favoritesKey, loadFavorites, saveFavorites, updateStoredFavorites } from './favorites.js';
import { moveFavoriteId, restoreFavoriteId, type FavoriteMoveDirection } from './favorites-order.js';
import { importDeviceLikes, setTrackLike } from './likes.js';
import { orderLibraryItems, type LibraryOrder } from './order.js';
import { loadRecentFinds, saveRecentFinds } from './recent-finds.js';
import { recentPlayTitles } from './recent-plays.js';
import { createLibraryRefresher, type LibraryRefreshUpdate } from './refresh.js';
import { appendSelection, hiddenSelectedCount, pruneSelection, selectAllDisplayed,
	selectRange, selectSelectedItems, toggleSelected } from './selection.js';
import { verifyLibrarySession } from './session-verification.js';
import { resolveStreamUrl } from './stream-url.js';
import { loadTrackTaste, saveTrackTaste, type TasteSignal } from './taste.js';

const binding = 'A'.repeat(43);

function memoryStorage(): SnapshotStorage {
	const data = new Map<string, string>();
	return {
		getItem: (key) => data.get(key) ?? null,
		setItem: (key, value) => { data.set(key, value); },
		removeItem: (key) => { data.delete(key); }
	};
}

function mockVerifiedSession(local: SnapshotStorage) {
	local.setItem(AUTH_GENERATION_KEY, 'current');
	vi.stubGlobal('window', { localStorage: local });
	vi.stubGlobal('caches', { keys: async () => [] });
	vi.stubGlobal('navigator', { locks: { request: async <T>(_name: string, _options: LockOptions, work: LockGrantedCallback<T>) => work({} as Lock) } });
	vi.stubGlobal('fetch', vi.fn(async () => Response.json({ session_binding: binding })));
	return { [SESSION_BINDING_HEADER]: binding };
}

afterEach(() => vi.unstubAllGlobals());

it('requires canonical bindings for persisted library provenance', () => {
	const session = memoryStorage();
	vi.stubGlobal('window', { localStorage: memoryStorage() });
	expect(establishVerifiedLibraryScope(session, 'unverified')).toBeUndefined();
	expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();
	session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'old-scope');
	session.setItem(LIBRARY_SESSION_BINDING_KEY, '["old-scope","unverified"]');
	expect(libraryScopeBinding(session)).toBeNull();
});

it('rejects a noncanonical snapshot timestamp and preserves the encoded format', () => {
	const raw = (savedAt: string) => JSON.stringify({ savedAt, items: [{ id: 'Track_A' }] });
	expect(decodeSnapshot(raw('2026-02-30T00:00:00.000Z'))).toBeNull();
	expect(decodeSnapshot(raw('2026-02-28T00:00:00.000Z'))?.items).toEqual([{ id: 'Track_A' }]);
});

it('clears provenance and cached bytes when context removal fails', () => {
	const session = memoryStorage();
	const keys = [LIBRARY_SESSION_BINDING_KEY, `${LIBRARY_SNAPSHOT_KEY}.old-scope`,
		`${LIBRARY_QUEUE_KEY}.old-scope`, LIBRARY_SNAPSHOT_KEY];
	session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'old-scope');
	for (const key of keys) session.setItem(key, 'value');
	clearSnapshot({ ...session, removeItem(key) {
		if (key === LIBRARY_AUTH_CONTEXT_KEY) throw new Error('Storage denied');
		session.removeItem(key);
	} });
	for (const key of keys) expect(session.getItem(key)).toBeNull();
});

it('publishes verified rows without inheriting unbound device storage', async () => {
	const session = memoryStorage(), local = memoryStorage();
	session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'old-scope');
	const oldKey = favoritesKey(session)!;
	local.setItem(oldKey, '["old-track"]');
	const headers = mockVerifiedSession(local);
	let update: LibraryRefreshUpdate | undefined;
	const refresher = createLibraryRefresher({ sessionStorage: () => session, invalidateLibraryCache: vi.fn(),
		onStateChange: (_, next) => { update = next; } });
	await refresher.refresh(async () => Response.json([{ id: 'Track_A' }], { headers }));
	expect(refresher.state.items).toEqual([{ id: 'Track_A' }]);
	const scope = update?.storageScope ?? null;
	expect(update).toMatchObject({ authoritative: true, sessionBinding: binding, deviceScope: favoritesKey(scope) });
	expect(favoritesKey(scope)).not.toBe(oldKey);
	expect(loadFavorites(scope, local)).toEqual([]);
	expect(await updateStoredFavorites(scope, local, update?.deviceScope ?? null, () => ['Track_A'])).toEqual(['Track_A']);
	expect(local.getItem(oldKey)).toBe('["old-track"]');
});

it.each(['stream', 'buffer'])('suspends signed-in refresh on malformed UTF-8 in a %s listing', async (mode) => {
	const session = memoryStorage();
	const headers = mockVerifiedSession(memoryStorage());
	const refresher = createLibraryRefresher({ sessionStorage: () => session, invalidateLibraryCache: vi.fn() });
	const refresh = () => refresher.refresh(async () => Response.json([{ id: 'Track_A' }], { headers }));
	await refresh();
	const key = `${LIBRARY_SNAPSHOT_KEY}.${session.getItem(LIBRARY_AUTH_CONTEXT_KEY)}`;
	const snapshot = session.getItem(key);
	expect(decodeSnapshot(snapshot)?.items).toEqual([{ id: 'Track_A' }]);
	const body = new TextEncoder().encode('[{"id":"?"}]');
	body[8] = 0xff;
	const response = new Response(body, { headers });
	if (mode === 'buffer') vi.spyOn(response, 'body', 'get').mockReturnValue(null);
	await refresher.refresh(async () => response);
	expect(refresher.state).toMatchObject({ items: [], verificationSuspended: true });
	expect(session.getItem(key)).toBe(snapshot);
	await refresh();
	expect(refresher.state).toMatchObject({ items: [{ id: 'Track_A' }], verificationSuspended: false });
});

it('withholds favorites fallback after the storage context ends', () => {
	const session = memoryStorage();
	const local = memoryStorage();
	expect(loadFavorites(session, local, ['old-track'])).toEqual([]);
	session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'current');
	expect(loadFavorites(session, null, ['current-track'])).toEqual(['current-track']);
});

it('keeps only distinct nonblank favorites and refuses malformed locked edits', async () => {
	const session = memoryStorage(), local = memoryStorage();
	session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'current');
	const key = favoritesKey(session)!;
	expect(saveFavorites(session, local, ['Track_A', ' ', 'Track_A'])).toBe(true);
	expect(local.getItem(key)).toBe('["Track_A"]');
	vi.stubGlobal('navigator', { locks: { request: async <T>(_name: string, _options: LockOptions, work: LockGrantedCallback<T>) => work({} as Lock) } });
	expect(await updateStoredFavorites(session, local, key, () => [' '])).toBeNull();
	expect(local.getItem(key)).toBe('["Track_A"]');
	local.setItem(key, '[" "]');
	expect(await updateStoredFavorites(session, local, key, () => ['Track_A'])).toBeNull();
	expect(loadFavorites(session, local)).toEqual([]);
});

it('keeps distinct recent finds within the persistence cap', () => {
	const session = memoryStorage(), local = memoryStorage();
	session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'current');
	saveRecentFinds(session, local, ['a', 'a', 'b', 'c', 'd', 'e']);
	expect(loadRecentFinds(session, local)).toEqual(['a', 'b', 'c', 'd', 'e']);
});

it('preserves ordering on unknown or malformed controls', () => {
	const stored = ['b', 'a'];
	expect(moveFavoriteId(stored, stored, 'b', 'sideways' as FavoriteMoveDirection)).toBe(stored);
	const blank = [' ', 'a'];
	expect(moveFavoriteId(blank, blank, ' ', 'down')).toBe(blank);
	expect(moveFavoriteId(blank, blank, 'a', 'up')).toBe(blank);
	expect(moveFavoriteId(stored, ['b', 'b'], 'b', 'down')).toBe(stored);
	expect(restoreFavoriteId(stored, 'c', { previousIndex: NaN, predecessor: null, successor: null })).toBe(stored);
	expect(restoreFavoriteId(stored, ' ', { previousIndex: 0, predecessor: null, successor: null })).toBe(stored);
	const tracks = stored.map((id) => ({ id, artist: id }));
	expect(orderLibraryItems(tracks, 'unexpected' as LibraryOrder)).toEqual(tracks);
	const rows = stored.map((id) => ({ id, title: id, kind: 'Artist' as const, creator: '', artPath: null }));
	expect(displayCollections(rows, '', 'unexpected' as CollectionSort, false)).toEqual(rows);
});

it('ignores blank row IDs and counts each selected ID once', () => {
	const malformed = Object.assign([], { id: 'array-row' });
	expect(toLibraryItems([malformed, { id: ' ' }, { id: 'Track_A' }])).toEqual([{ id: 'Track_A' }]);
	const stored = ['b', 'a', ' '];
	expect(pruneSelection(['a', 'a', 'b', ' '], stored)).toEqual(['a', 'b']);
	expect(hiddenSelectedCount(['a', 'a', 'b'], ['b'])).toBe(1);
});

it('keeps blank IDs out of selection projections and queue actions', () => {
	const selection = ['a'];
	const visible = ['a', ' ', 'b'];
	const rows = visible.map((id) => ({ id }));
	const queue = { tracks: [], currentIndex: -1 };
	expect(toggleSelected(selection, ' ')).toBe(selection);
	expect(selectRange(selection, visible, 'a', ' ')).toBe(selection);
	expect(selectRange(selection, visible, ' ', 'a')).toEqual([]);
	expect(selectRange(selection, visible, 'a', 'b')).toEqual(['a', 'b']);
	expect(selectAllDisplayed(selection, visible)).toEqual(['a', 'b']);
	expect(selectSelectedItems(rows, [' '])).toEqual([]);
	expect(appendSelection(queue, [' '], rows)).toEqual({ queue, added: 0, skipped: 0 });
});

it('rejects unknown collection filters before dispatch', async () => {
	const get = vi.fn(async () => []);
	await expect(loadCollections({ get }, 'unexpected' as CollectionFilter)).rejects.toThrow();
	expect(get).not.toHaveBeenCalled();
});

it('derives listening-history job aliases only from complete catalogue occurrence keys', () => {
	const row = { id: 'recording', recording_mbids: ['recording'], title: 'Song' };
	for (const occurrence_key of ['OtherJob', 'OtherJob:1:extra', 'private:recording']) {
		expect([...recentPlayTitles([{ ...row, occurrence_key }])]).toEqual([['recording', 'Song']]);
	}
	expect(recentPlayTitles([{ ...row, occurrence_key: 'Job_A:1' }]).get('Job_A')).toBe('Song');
});

it('refuses malformed preference requests before dispatch', async () => {
	const client = { get: vi.fn(), put: vi.fn(), delete: vi.fn() } satisfies Pick<ApiClient, 'get' | 'put' | 'delete'>;
	await expect(loadTrackTaste(client, '')).rejects.toThrow();
	await expect(saveTrackTaste(client, binding, 'track/path', 'hide')).rejects.toThrow();
	await expect(saveTrackTaste(client, binding, 'track', 'unexpected' as TasteSignal)).rejects.toThrow();
	await expect(setTrackLike(client, 'track', 'false' as unknown as boolean)).rejects.toThrow();
	expect(client.get).not.toHaveBeenCalled();
	expect(client.put).not.toHaveBeenCalled();
	expect(client.delete).not.toHaveBeenCalled();
});

it('refuses device likes import without a canonical binding', async () => {
	const post = vi.fn().mockResolvedValue({ added: 1, already: 0, rejected: [] });
	const retireConfirmed = vi.fn(async () => true);
	const run = async <T>(work: () => T) => work();
	await expect(importDeviceLikes({ post }, ['Track_A'], { binding: '', isCurrent: () => true, run, retireConfirmed })).resolves.toBeNull();
	expect(post).not.toHaveBeenCalled();
	expect(retireConfirmed).not.toHaveBeenCalled();
});

it('rejects malformed release taste entries alongside valid track preferences', async () => {
	const get = vi.fn().mockResolvedValue({ session_binding: binding, items: [
		{ kind: 'track', target_id: 'Track_A', signal: 'hide', until: null },
		{ kind: 'release', target_id: 'bad', signal: 'bad' }
	] });
	await expect(loadTrackTaste({ get }, binding)).rejects.toThrow();
	get.mockResolvedValue({ session_binding: binding, items: [
		{ kind: 'track', target_id: 'Track_A', signal: 'hide', until: null },
		{ kind: 'release', target_id: '00000000-0000-4000-8000-000000000001', signal: 'love', until: null }
	] });
	await expect(loadTrackTaste({ get }, binding)).resolves.toEqual({
		tracks: new Map([['Track_A', { signal: 'hide', until: null }]]), complete: true
	});
});

it('does not dispatch session verification with a malformed expected binding', async () => {
	const fetch = vi.fn(async () => Response.json({ session_binding: binding }));
	vi.stubGlobal('fetch', fetch);
	const { signal } = new AbortController();
	const owner = { generation: 'test', signal, isCurrent: () => true, run: async <T>(work: () => T) => work() };
	await expect(verifyLibrarySession('', signal, owner)).resolves.toBe(false);
	expect(fetch).not.toHaveBeenCalled();
});

it('accepts application-relative media paths and rejects other URL forms', () => {
	for (const streamUrl of ['//media.example.test/a', '/\\media.example.test/a', '/\t/media.example.test/a',
		'https://media.example.test/a', 'data:audio/wav,abc', '?track=1']) {
		expect(resolveStreamUrl({ streamUrl }), JSON.stringify(streamUrl)).toBe('');
	}
	expect(resolveStreamUrl({ streamUrl: '  /api/demo/audio?track=1  ' })).toBe('/api/demo/audio?track=1');
});
