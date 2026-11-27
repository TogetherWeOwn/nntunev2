import { afterEach, expect, it, vi } from 'vitest';
import type { ApiClient } from '../api/client.js';
import { AUTH_GENERATION_KEY, SESSION_BINDING_HEADER } from '../auth-fence.js';
import {
	clearSnapshot, decodeSnapshot, establishVerifiedLibraryScope, LIBRARY_AUTH_CONTEXT_KEY,
	LIBRARY_QUEUE_KEY, LIBRARY_SESSION_BINDING_KEY, LIBRARY_SNAPSHOT_KEY, libraryScopeBinding,
	toLibraryItems, type SnapshotStorage
} from './cache.js';
import { loadCollections, type CollectionFilter } from './collections.js';
import { loadFavorites } from './favorites.js';
import { moveFavoriteId, restoreFavoriteId, type FavoriteMoveDirection } from './favorites-order.js';
import { setTrackLike } from './likes.js';
import { orderLibraryItems, type LibraryOrder } from './order.js';
import { loadRecentFinds, saveRecentFinds } from './recent-finds.js';
import { recentPlayTitles } from './recent-plays.js';
import { createLibraryRefresher, type LibraryRefreshUpdate } from './refresh.js';
import { hiddenSelectedCount, pruneSelection } from './selection.js';
import { verifyLibrarySession } from './session-verification.js';
import { resolveStreamUrl } from './stream-url.js';
import { loadTrackTaste, saveTrackTaste } from './taste.js';

const binding = 'A'.repeat(43);

function memoryStorage(): SnapshotStorage {
	const data = new Map<string, string>();
	return {
		getItem: (key) => data.get(key) ?? null,
		setItem: (key, value) => { data.set(key, value); },
		removeItem: (key) => { data.delete(key); }
	};
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
	local.setItem(AUTH_GENERATION_KEY, 'current');
	vi.stubGlobal('window', { localStorage: local });
	vi.stubGlobal('caches', { keys: async () => [] });
	vi.stubGlobal('navigator', { locks: { request: async (_name: string, _options: LockOptions, work: LockGrantedCallback) => work({} as Lock) } });
	vi.stubGlobal('fetch', vi.fn(async () => Response.json({ session_binding: binding })));
	let update: LibraryRefreshUpdate | undefined;
	const refresher = createLibraryRefresher({ sessionStorage: () => session, invalidateLibraryCache: vi.fn(),
		onStateChange: (_, next) => { update = next; } });
	await refresher.refresh(async () => Response.json([{ id: 'Track_A' }], { headers: { [SESSION_BINDING_HEADER]: binding } }));
	expect(refresher.state.items).toEqual([{ id: 'Track_A' }]);
	expect(update).toMatchObject({ authoritative: true, sessionBinding: binding, storageScope: null });
	expect(session.getItem(`${LIBRARY_SNAPSHOT_KEY}.old-scope`)).toBeNull();
});

it.each(['stream', 'buffer'])('rejects malformed UTF-8 in a %s listing before snapshot publication', async (mode) => {
	const session = memoryStorage();
	const refresher = createLibraryRefresher({ sessionStorage: () => session, invalidateLibraryCache: vi.fn() });
	const body = new TextEncoder().encode('[{"id":"?"}]');
	body[8] = 0xff;
	const response = new Response(body);
	if (mode === 'buffer') vi.spyOn(response, 'body', 'get').mockReturnValue(null);
	await refresher.refresh(async () => response);
	expect(refresher.state).toMatchObject({ items: [], loadError: 'Library is not reachable right now.' });
	expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();
});

it('withholds favorites fallback after the storage context ends', () => {
	const session = memoryStorage();
	const local = memoryStorage();
	expect(loadFavorites(session, local, ['old-track'])).toEqual([]);
	session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'current');
	expect(loadFavorites(session, null, ['current-track'])).toEqual(['current-track']);
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
	expect(restoreFavoriteId(stored, 'c', { previousIndex: NaN, predecessor: null, successor: null })).toBe(stored);
	const tracks = stored.map((id) => ({ id, artist: id }));
	expect(orderLibraryItems(tracks, 'unexpected' as LibraryOrder)).toEqual(tracks);
});

it('ignores blank row IDs and counts each selected ID once', () => {
	expect(toLibraryItems([{ id: ' ' }, { id: 'Track_A' }])).toEqual([{ id: 'Track_A' }]);
	const stored = ['b', 'a', ' '];
	expect(pruneSelection(['a', 'a', 'b', ' '], stored)).toEqual(['a', 'b']);
	expect(hiddenSelectedCount(['a', 'a', 'b'], ['b'])).toBe(1);
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
	await expect(setTrackLike(client, 'track', 'false' as unknown as boolean)).rejects.toThrow();
	expect(client.get).not.toHaveBeenCalled();
	expect(client.put).not.toHaveBeenCalled();
	expect(client.delete).not.toHaveBeenCalled();
});

it('rejects malformed release taste entries alongside valid track preferences', async () => {
	const get = vi.fn().mockResolvedValue({ session_binding: binding, items: [
		{ kind: 'track', target_id: 'Track_A', signal: 'hide', until: null },
		{ kind: 'release', target_id: 'bad', signal: 'bad' }
	] });
	await expect(loadTrackTaste({ get }, binding)).rejects.toThrow();
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
