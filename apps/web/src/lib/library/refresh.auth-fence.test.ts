import { afterEach, expect, it, vi } from 'vitest';
import { ApiClient } from '../api/client.js';
import { AUTH_GENERATION_KEY, captureAuthFence } from '../auth-fence.js';
import { LIBRARY_AUTH_CONTEXT_KEY, LIBRARY_FAVORITES_KEY, LIBRARY_SESSION_BINDING_KEY, loadSnapshot, type SnapshotStorage } from './cache.js';
import { createLibraryRefresher, type LibraryRefreshUpdate } from './refresh.js';

function setup(verifySession = vi.fn(async () => true)) {
	const lock = { held: false, foreign: false };
	const storage = (): SnapshotStorage => {
		const entries = new Map<string, string>();
		return { getItem: (key) => entries.get(key) ?? null, setItem: (key, value) => entries.set(key, value),
			removeItem: (key) => entries.delete(key) };
	};
	const local = storage();
	const session = storage();
	vi.stubGlobal('window', { localStorage: local, sessionStorage: session });
	vi.stubGlobal('navigator', { locks: { async request(_: string, _options: unknown, work: (lock: object | null) => unknown) {
		if (lock.held || lock.foreign) return work(null);
		lock.held = true;
		try { return await work({}); } finally { lock.held = false; }
	} } });
	const updates: LibraryRefreshUpdate[] = [];
	const refresher = createLibraryRefresher({ sessionStorage: () => session, invalidateLibraryCache: vi.fn(),
		verifySession, onStateChange: (_, update) => updates.push(update) });
	const refresh = () => refresher.refresh(async () => Response.json([{ id: 'track' }], {
		headers: { 'x-nntune-session-binding': 'current' }
	}));
	return { local, session, lock, refresh, refresher, updates };
}

afterEach(() => vi.unstubAllGlobals());

it.each(['generation changed', 'writer holds commit lock'])('rejects a matching verification when %s', async (event) => {
	const started = Promise.withResolvers<void>();
	const reply = Promise.withResolvers<boolean>();
	const { local, session, lock, refresh, refresher, updates } = setup(vi.fn(() => {
		started.resolve();
		return reply.promise;
	}));
	const context = event === 'generation changed' ? 'existing-scope' : null;
	if (context) session.setItem(LIBRARY_AUTH_CONTEXT_KEY, context);
	if (context) session.setItem(LIBRARY_SESSION_BINDING_KEY, JSON.stringify([context, 'current']));
	const pending = refresh();
	await started.promise;
	if (event === 'generation changed') local.setItem(AUTH_GENERATION_KEY, 'replacement');
	else lock.foreign = true;
	reply.resolve(true);
	await pending;
	expect(loadSnapshot(session)).toBeNull();
	expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBe(context);
	expect(refresher.state.items).toEqual([]);
	expect(updates.every(({ sessionBinding }) => !sessionBinding)).toBe(true);
});

it('verifies afresh after a writer disappears or a rejected auth POST exhausts storage quota', async () => {
	const verify = vi.fn(async () => true);
	const { local, lock, refresh, updates } = setup(verify);
	lock.foreign = true;
	await refresh();
	expect(verify).not.toHaveBeenCalled();
	lock.foreign = false; // The browser releases a closed document's lock.
	await refresh();
	const { deviceScope, isCurrent } = updates.at(-1)!;
	expect(deviceScope).toMatch(/^nntune\.library\.favorites\.v1\./);
	local.setItem(deviceScope!, ' [ "track" ] ');
	vi.spyOn(local, 'setItem').mockImplementationOnce(() => { throw new DOMException('full', 'QuotaExceededError'); });
	const client = new ApiClient({ baseUrl: '', fetchFn: async () => {
		expect(lock.held).toBe(true);
		expect(isCurrent()).toBe(false);
		return Response.json({}, { status: 500 });
	} });
	await expect(client.logout()).rejects.toMatchObject({ status: 500 });
	await refresh();
	expect(updates.at(-1)?.deviceScope).toBe(deviceScope);
	expect(local.getItem(deviceScope!)).toBe(' [ "track" ] ');
	expect(isCurrent()).toBe(false);
});

it('does not bind or rewrite an existing unbound list', async () => {
	const { local, session, refresh, updates } = setup();
	const key = `${LIBRARY_FAVORITES_KEY}.legacy`;
	session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'legacy');
	local.setItem(key, ' [ "track" ] ');
	await refresh();
	expect(updates.at(-1)).toMatchObject({ sessionBinding: 'current', deviceScope: undefined });
	expect(local.getItem(key)).toBe(' [ "track" ] ');
});

it('dispatches ordinary auth but refuses verified Library when storage is blocked', async () => {
	const verify = vi.fn(async () => true);
	const { refresh } = setup(verify);
	vi.stubGlobal('window', { get localStorage() { throw new Error('Blocked'); } });
	await new ApiClient({ baseUrl: '', fetchFn: async () => Response.json({}) }).logout();
	await refresh();
	expect(await captureAuthFence()).toBeUndefined();
	expect(verify).not.toHaveBeenCalled();
});
