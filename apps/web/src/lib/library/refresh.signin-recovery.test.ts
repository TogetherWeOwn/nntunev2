import { describe, expect, it } from 'vitest';

import {
	LIBRARY_AUTH_TRANSITION_KEY,
	loadSnapshot,
	type SnapshotStorage
} from '#lib/library/cache.js';
import {
	createLibraryRefresher,
	type LibraryRefreshState,
	type LibraryRefresher
} from '#lib/library/refresh.js';

/**
 * Acceptance: sign-in recovery from library auth failures is typed state,
 * not message matching.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * implementation on top of this branch and must make these tests pass
 * WITHOUT editing this file; a wrong test comes back to the acceptance
 * card instead. Browser behavior is pinned in
 * `apps/web/tests/library-signin-recovery.spec.ts`.
 *
 * Contract under test (paired build):
 * - `LibraryRefreshState` gains a typed `requiresSignIn: boolean` field.
 *   It starts `false`, is reset to `false` when a refresh starts, and is
 *   `true` after a 401 or a cross-tab auth transition — the two events
 *   where signing in again is the recovery. It stays `false` for 403,
 *   404, 5xx, network and timeout failures, and after any success.
 * - The field is read here through a narrow cast so this suite compiles
 *   and runs against the pre-build state shape: before the build the
 *   value is `undefined`, which is exactly the RED these tests record.
 *   The build must add the real field; renaming it leaves this suite red.
 * - 403 keeps an error message but the wording stays neutral about the
 *   session: a forbidden response must not claim the user is signed out
 *   or that their session expired, and must not be the 401 message.
 * - Existing privacy behavior is unchanged: 401/403 and auth transitions
 *   still invalidate the cache and clear shown items, a prior account's
 *   tracks never reappear from a late or superseded refresh, and the
 *   snapshot fallback after an authenticated success still works.
 * - The library page mirrors the typed field from refresher state and
 *   gates a real `/login` anchor on it; it never derives the sign-in
 *   action from the error message text.
 */

const FAST_TIMEOUT_MS = 25;
const SLOW_TIMEOUT_MS = 5_000;

/** Read the typed sign-in-recovery flag the paired build must add. */
function requiresSignIn(state: LibraryRefreshState): boolean | undefined {
	return (state as Partial<{ requiresSignIn: boolean }>).requiresSignIn;
}

function memoryStorage(): SnapshotStorage {
	const map = new Map<string, string>();
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

function makeRefresher(
	timeoutMs: number = FAST_TIMEOUT_MS,
	onStateChange?: (state: LibraryRefreshState) => void
): { refresher: LibraryRefresher; storage: SnapshotStorage; invalidations: () => number } {
	const storage = memoryStorage();
	let invalidateCount = 0;
	const refresher = createLibraryRefresher({
		sessionStorage: () => storage,
		invalidateLibraryCache: () => {
			invalidateCount += 1;
		},
		onStateChange,
		timeoutMs
	});
	return { refresher, storage, invalidations: () => invalidateCount };
}

interface Deferred<T> {
	promise: Promise<T>;
	resolve: (value: T) => void;
}

function deferred<T>(): Deferred<T> {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((res) => {
		resolve = res;
	});
	return { promise, resolve };
}

function jsonResponse(body: string, status = 200): Response {
	return new Response(body, { status, headers: { 'content-type': 'application/json' } });
}

function okFetch(payload: unknown = [{ id: 'mine', title: 'My track' }]): typeof fetch {
	return (async () => jsonResponse(JSON.stringify(payload))) as typeof fetch;
}

function statusFetch(status: number): typeof fetch {
	return (async () => jsonResponse('{"error":"denied"}', status)) as typeof fetch;
}

describe('typed sign-in recovery state', () => {
	it('starts false before any refresh', () => {
		const { refresher } = makeRefresher();

		expect(requiresSignIn(refresher.state)).toBe(false);
	});

	it('an initial 401 requires sign-in, invalidates the cache, and clears items', async () => {
		const { refresher, invalidations } = makeRefresher();

		await refresher.refresh(statusFetch(401));

		expect(requiresSignIn(refresher.state)).toBe(true);
		expect(refresher.state.loadError).not.toBeNull();
		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.staleAt).toBeNull();
		expect(refresher.state.loading).toBe(false);
		expect(invalidations()).toBe(1);
	});

	it('a 401 after a successful load requires sign-in and drops the prior tracks', async () => {
		const { refresher, invalidations } = makeRefresher();
		await refresher.refresh(okFetch());
		expect(refresher.state.items.map((i) => i.id)).toEqual(['mine']);
		expect(requiresSignIn(refresher.state)).toBe(false);

		await refresher.refresh(statusFetch(401));

		expect(requiresSignIn(refresher.state)).toBe(true);
		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.staleAt).toBeNull();
		expect(invalidations()).toBe(1);
	});

	it('prior tracks never reappear when a refresh after a 401 fails on the network', async () => {
		const { refresher } = makeRefresher();
		await refresher.refresh(okFetch());
		await refresher.refresh(statusFetch(401));
		expect(refresher.state.items).toEqual([]);

		await refresher.refresh((async () => {
			throw new Error('network down');
		}) as typeof fetch);

		// The 401 revoked this load's trust in the snapshot: the earlier
		// account's tracks must not come back through the fallback path.
		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.staleAt).toBeNull();
	});

	it('a cross-tab auth transition requires sign-in and clears items and snapshot', async () => {
		const { refresher, storage } = makeRefresher();
		await refresher.refresh(okFetch());
		expect(loadSnapshot(storage)?.items.map((i) => i.id)).toEqual(['mine']);

		refresher.handleAuthTransition({ key: LIBRARY_AUTH_TRANSITION_KEY });

		expect(requiresSignIn(refresher.state)).toBe(true);
		expect(refresher.state.loadError).toBe(
			'Your session changed. Sign in again to see this library.'
		);
		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.staleAt).toBeNull();
		expect(refresher.state.loading).toBe(false);
		expect(loadSnapshot(storage)).toBeNull();
	});

	it('a late success superseded by an auth transition cannot resurrect prior tracks', async () => {
		const { refresher } = makeRefresher(SLOW_TIMEOUT_MS);
		await refresher.refresh(okFetch());

		const late = deferred<Response>();
		const refreshPromise = refresher.refresh((() => late.promise) as unknown as typeof fetch);
		refresher.handleAuthTransition({ key: LIBRARY_AUTH_TRANSITION_KEY });
		late.resolve(jsonResponse('[{"id":"mine","title":"My track"}]'));
		await refreshPromise;

		expect(refresher.state.items).toEqual([]);
		expect(requiresSignIn(refresher.state)).toBe(true);
		expect(refresher.state.loadError).toBe(
			'Your session changed. Sign in again to see this library.'
		);
	});

	it('mirrors the typed flag through onStateChange so the page can react to it', async () => {
		const seen: Array<boolean | undefined> = [];
		const { refresher } = makeRefresher(FAST_TIMEOUT_MS, (state) =>
			seen.push(requiresSignIn(state))
		);

		await refresher.refresh(statusFetch(401));

		expect(seen.length).toBeGreaterThan(0);
		expect(seen[seen.length - 1]).toBe(true);
	});

	it('resets to false when a new refresh starts and stays false after it succeeds', async () => {
		const { refresher } = makeRefresher(SLOW_TIMEOUT_MS);
		await refresher.refresh(statusFetch(401));
		expect(requiresSignIn(refresher.state)).toBe(true);

		const late = deferred<Response>();
		const refreshPromise = refresher.refresh((() => late.promise) as unknown as typeof fetch);
		// The stale sign-in recovery must not linger under the next attempt.
		expect(requiresSignIn(refresher.state)).toBe(false);
		expect(refresher.state.loading).toBe(true);

		late.resolve(jsonResponse('[{"id":"fresh","title":"Fresh track"}]'));
		await refreshPromise;
		expect(requiresSignIn(refresher.state)).toBe(false);
		expect(refresher.state.loadError).toBeNull();
		expect(refresher.state.items.map((i) => i.id)).toEqual(['fresh']);
	});

	it('a 403 after a 401 clears the sign-in recovery', async () => {
		const { refresher } = makeRefresher();
		await refresher.refresh(statusFetch(401));
		expect(requiresSignIn(refresher.state)).toBe(true);

		await refresher.refresh(statusFetch(403));

		expect(requiresSignIn(refresher.state)).toBe(false);
		expect(refresher.state.items).toEqual([]);
	});

	it('an unrelated storage event does not clear an active sign-in recovery', async () => {
		const { refresher } = makeRefresher();
		await refresher.refresh(statusFetch(401));
		expect(requiresSignIn(refresher.state)).toBe(true);

		refresher.handleAuthTransition({ key: 'some-other-key' });

		expect(requiresSignIn(refresher.state)).toBe(true);
		expect(refresher.state.loadError).not.toBeNull();
	});
});

describe('failures that must not offer sign-in recovery', () => {
	it('403 clears private state with session-neutral wording and no sign-in flag', async () => {
		const { refresher, invalidations } = makeRefresher();
		await refresher.refresh(okFetch());

		await refresher.refresh(statusFetch(403));

		expect(requiresSignIn(refresher.state)).toBe(false);
		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.staleAt).toBeNull();
		expect(refresher.state.loading).toBe(false);
		expect(invalidations()).toBe(1);
		const message = refresher.state.loadError;
		expect(message).not.toBeNull();
		// A forbidden response says nothing about the session: the copy
		// must not claim sign-out, expiry, or ask the user to sign in.
		expect(message!).not.toMatch(/signed out|sign in|expired/i);
	});

	it('the 403 message is not the 401 message', async () => {
		const unauthorized = makeRefresher();
		await unauthorized.refresher.refresh(statusFetch(401));
		const forbidden = makeRefresher();
		await forbidden.refresher.refresh(statusFetch(403));

		expect(unauthorized.refresher.state.loadError).not.toBeNull();
		expect(forbidden.refresher.state.loadError).not.toBeNull();
		expect(forbidden.refresher.state.loadError).not.toBe(
			unauthorized.refresher.state.loadError
		);
	});

	it('404 keeps the neutral unavailable state with no sign-in flag', async () => {
		const { refresher } = makeRefresher();
		await refresher.refresh(okFetch());

		await refresher.refresh(statusFetch(404));

		expect(requiresSignIn(refresher.state)).toBe(false);
		expect(refresher.state.libraryUnavailable).toBe(true);
		expect(refresher.state.loadError).toBeNull();
		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.loading).toBe(false);
	});

	it('a network failure keeps the unreachable error with no sign-in flag', async () => {
		const { refresher } = makeRefresher();

		await refresher.refresh((async () => {
			throw new Error('network down');
		}) as typeof fetch);

		expect(requiresSignIn(refresher.state)).toBe(false);
		expect(refresher.state.loadError).toBe('Library is not reachable right now.');
		expect(refresher.state.items).toEqual([]);
	});

	it('a timeout keeps the unreachable error with no sign-in flag', async () => {
		const { refresher } = makeRefresher();
		const never = new Promise<Response>(() => {});

		await refresher.refresh((() => never) as unknown as typeof fetch);

		expect(requiresSignIn(refresher.state)).toBe(false);
		expect(refresher.state.loadError).toBe('Library is not reachable right now.');
		expect(refresher.state.loading).toBe(false);
	});

	it('a 500 keeps the unreachable error with no sign-in flag', async () => {
		const { refresher } = makeRefresher();

		await refresher.refresh(statusFetch(500));

		expect(requiresSignIn(refresher.state)).toBe(false);
		expect(refresher.state.loadError).toBe('Library is not reachable right now.');
		expect(refresher.state.items).toEqual([]);
	});

	it('the snapshot fallback after an authenticated success carries no sign-in flag', async () => {
		const { refresher } = makeRefresher();
		await refresher.refresh(okFetch());

		await refresher.refresh((async () => {
			throw new Error('network down');
		}) as typeof fetch);

		expect(requiresSignIn(refresher.state)).toBe(false);
		expect(refresher.state.items.map((i) => i.id)).toEqual(['mine']);
		expect(refresher.state.staleAt).not.toBeNull();
		expect(refresher.state.loadError).toBeNull();
	});
});

// The library page is client-fetched (`ssr = false`), so the typed-state
// wiring is pinned against the route source (same raw-glob approach as
// `src/routes/login/page.test.ts`); the rendered behavior is pinned in
// `apps/web/tests/library-signin-recovery.spec.ts`.
const pageSources = import.meta.glob('../../routes/library/+page.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

function readLibraryPage(): string {
	const source = Object.values(pageSources)[0];
	if (typeof source !== 'string' || source.length === 0) {
		throw new Error(
			'missing library page source: src/routes/library/+page.svelte was not found by the raw glob'
		);
	}
	return source;
}

describe('library page sign-in recovery wiring', () => {
	it('mirrors the typed requiresSignIn flag from refresher state', () => {
		const source = readLibraryPage();

		expect(source).toMatch(/state\.requiresSignIn/);
	});

	it('renders a real /login anchor gated on the typed flag', () => {
		const source = readLibraryPage();

		// A genuine anchor (keyboard-activatable, middle-clickable) to the
		// existing login route, rendered only under the typed flag.
		expect(source).toMatch(/<a\s[^>]*href="\/login"[^>]*>/);
		expect(source).toMatch(/requiresSignIn[\s\S]{0,600}href="\/login"/);
	});

	it('never derives the sign-in action from the error message text', () => {
		const source = readLibraryPage();

		expect(source).not.toMatch(/loadError\.(?:includes|startsWith|endsWith|indexOf|match)\s*\(/);
		expect(source).not.toMatch(/loadError\s*===?\s*['"`]/);
	});
});
