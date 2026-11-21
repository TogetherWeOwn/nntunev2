import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiClient } from '../api/client.js';
import { AUTH_GENERATION_KEY, captureAuthFence } from '../auth-fence.js';
import {
	LIBRARY_AUTH_CONTEXT_KEY,
	LIBRARY_FAVORITES_KEY,
	LIBRARY_SESSION_BINDING_KEY,
	loadSnapshot,
	verifiedFavoritesKey,
	type SnapshotStorage
} from './cache.js';
import { loadFavorites, saveFavorites } from './favorites.js';
import { createLibraryRefresher, type LibraryRefreshUpdate } from './refresh.js';

function storage(): SnapshotStorage {
	const entries: Record<string, string> = {};
	return Object.defineProperties(entries, {
		getItem: { value: (key: string) => entries[key] ?? null },
		setItem: { value: (key: string, value: string) => { entries[key] = value; } },
		removeItem: { value: (key: string) => { delete entries[key]; } }
	}) as unknown as SnapshotStorage;
}

function setup(verifySession?: (binding: string, signal: AbortSignal) => Promise<boolean>) {
	const local = storage();
	const session = storage();
	vi.stubGlobal('window', { localStorage: local, sessionStorage: session });
	const updates: LibraryRefreshUpdate[] = [];
	const refresher = createLibraryRefresher({
		sessionStorage: () => session,
		invalidateLibraryCache: vi.fn(),
		verifySession,
		onStateChange: (_, update) => updates.push(update)
	});
	return { local, session, refresher, updates };
}

const library = (binding: string) => async () => Response.json([{ id: 'track' }], {
	headers: { 'x-nntune-session-binding': binding }
});

const pendingKey = `${AUTH_GENERATION_KEY}.pending.other-tab`;
afterEach(() => vi.unstubAllGlobals());

describe('verified Library refresh', () => {
	it('rejects an otherwise matching deferred verification without a delivered auth event', async () => {
		const started = Promise.withResolvers<void>();
		const matches = Promise.withResolvers<boolean>();
		const { local, session, refresher, updates } = setup(() => {
			started.resolve();
			return matches.promise;
		});
		const refresh = refresher.refresh(library('outgoing'));
		await started.promise;
		// Another tab begins and completes a replacement; no StorageEvent is delivered.
		local.setItem(pendingKey, '');
		local.setItem(AUTH_GENERATION_KEY, 'replacement');
		const startedDuring = captureAuthFence();
		local.removeItem(pendingKey);
		matches.resolve(true);
		await refresh;
		expect(startedDuring()).toBe(false);
		expect(refresher.state).toMatchObject({ items: [], requiresSignIn: true, loading: false });
		expect(loadSnapshot(session)).toBeNull();
		expect(updates.at(-1)).toMatchObject({ authBoundary: true });
		expect(updates.every(({ sessionBinding, deviceScope }) => !sessionBinding && !deviceScope)).toBe(true);
	});

	it('does not start session verification while another tab has a pending replacement', async () => {
		const verify = vi.fn(async () => true);
		const { local, refresher } = setup(verify);
		local.setItem(pendingKey, '');
		await refresher.refresh(library('outgoing'));
		expect(verify).not.toHaveBeenCalled();
		expect(refresher.state.items).toEqual([]);
	});

	it('keeps an existing unbound list on-device without changing its exact bytes', async () => {
		const { local, session, refresher, updates } = setup(async () => true);
		const key = `${LIBRARY_FAVORITES_KEY}.legacy`;
		const raw = ' [ "outgoing" ]\n';
		session.setItem(LIBRARY_AUTH_CONTEXT_KEY, 'legacy');
		local.setItem(key, raw);
		await refresher.refresh(library('replacement'));
		expect(updates.at(-1)?.sessionBinding).toBe('replacement');
		expect(updates.at(-1)?.deviceScope).toBeUndefined();
		expect(verifiedFavoritesKey(session, 'replacement')).toBeNull();
		expect(session.getItem(LIBRARY_SESSION_BINDING_KEY)).toBeNull();
		expect(loadFavorites(session, local)).toEqual(['outgoing']);
		expect(local.getItem(key)).toBe(raw);
	});

	it('binds a fresh scope in ordinary Library, then revokes it on a binding mismatch', async () => {
		const { local, session, refresher, updates } = setup();
		const verifier = vi.fn(async () => Response.json({ session_binding: 'current' }));
		vi.stubGlobal('fetch', verifier);
		await refresher.refresh(library('current'));
		expect(verifier).toHaveBeenCalledWith(expect.stringMatching(/\/v1\/me\/likes\/session$/),
			expect.objectContaining({ method: 'GET', credentials: 'include' }));
		const { deviceScope, sessionBinding, isCurrent } = updates.at(-1)!;
		expect(sessionBinding).toBe('current');
		expect(deviceScope).toMatch(/^nntune\.library\.favorites\.v1\./);
		expect(verifiedFavoritesKey(session, 'current')).toBe(deviceScope);
		expect(saveFavorites(session, local, ['track'])).toBe(true);

		verifier.mockImplementation(async () => Response.json({ session_binding: 'replacement' }));
		await refresher.refresh(library('replacement'));
		expect(refresher.state.requiresSignIn).toBe(true);
		expect(isCurrent()).toBe(false);
		expect(session.getItem(LIBRARY_SESSION_BINDING_KEY)).toBeNull();
		expect(verifiedFavoritesKey(session, 'current')).toBeNull();
		expect(local.getItem(deviceScope!)).toBe('["track"]');
		await refresher.refresh(library('replacement'));
		expect(updates.at(-1)?.deviceScope).toBe(verifiedFavoritesKey(session, 'replacement'));
		expect(updates.at(-1)?.deviceScope).not.toBe(deviceScope);
		expect(loadFavorites(session, local)).toEqual([]);
	});
});

describe('browser auth writes', () => {
	it.each([
		'/v1/auth/magic/finish',
		'/v1/auth/passkey/register/finish',
		'/v1/auth/passkey/login/finish',
		'/v1/session/reauthenticate/finish',
		'/v1/session/refresh',
		'/v1/session/logout',
		'/v1/session/revoke-all'
	])('marks %s before dispatch, even if the write fails', async (path) => {
		setup();
		const before = captureAuthFence();
		expect(before()).toBe(true);
		const client = new ApiClient({ baseUrl: '', fetchFn: async () => {
			expect(before()).toBe(false);
			expect(captureAuthFence()()).toBe(false);
			return Response.json({}, { status: 500 });
		} });
		await expect(client.post(path, {})).rejects.toMatchObject({ status: 500 });
		expect(before()).toBe(false);
		expect(captureAuthFence()()).toBe(true);
	});

	it('keeps the fence closed until the last overlapping request settles', async () => {
		const { local } = setup();
		const first = Promise.withResolvers<Response>();
		const last = Promise.withResolvers<Response>();
		const client = new ApiClient({ baseUrl: '', fetchFn: vi.fn()
			.mockReturnValueOnce(first.promise).mockReturnValueOnce(last.promise) });
		const a = client.logout();
		const b = client.revokeAllSessions();
		const pending = () => Object.keys(local).filter((key) => key.includes('.pending.')).length;
		expect(pending()).toBe(2);
		first.resolve(Response.json({ ok: true }));
		await a;
		expect(pending()).toBe(1);
		expect(captureAuthFence()()).toBe(false);
		last.resolve(Response.json({ ok: true }));
		await b;
		expect(pending()).toBe(0);
		expect(captureAuthFence()()).toBe(true);
	});

	it('allows the auth request but no trusted operation when storage is unavailable', async () => {
		vi.resetModules();
		const { ApiClient: Client } = await import('../api/client.js');
		const { captureAuthFence: capture } = await import('../auth-fence.js');
		vi.stubGlobal('window', { get localStorage() { throw new Error('Blocked'); } });
		expect(capture()()).toBe(false);
		const fetchFn = vi.fn(async () => Response.json({ ok: true }));
		await expect(new Client({ baseUrl: '', fetchFn }).logout()).resolves.toBeUndefined();
		expect(fetchFn).toHaveBeenCalledOnce();
		expect(capture()()).toBe(false);
	});
});
