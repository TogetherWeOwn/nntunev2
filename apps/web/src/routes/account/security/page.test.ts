import { describe, expect, it, vi } from 'vitest';
import { render } from 'svelte/server';
import SecurityPage from './+page.svelte';
import { load } from './+page';
import {
	LIBRARY_AUTH_CONTEXT_KEY,
	LIBRARY_AUTH_TRANSITION_KEY,
	LIBRARY_SNAPSHOT_KEY,
	type SnapshotStorage
} from '#lib/library/cache.js';

function memoryStorage(seed: Record<string, string> = {}): SnapshotStorage & {
	map: Map<string, string>;
} {
	const map = new Map(Object.entries(seed));
	return {
		map,
		getItem: (key) => map.get(key) ?? null,
		setItem: (key, value) => {
			map.set(key, value);
		},
		removeItem: (key) => {
			map.delete(key);
		}
	};
}

function sessionFetch(status: number): typeof fetch {
	return (async () => new Response(status === 200 ? '{}' : '{"error":"request failed"}', { status })) as typeof fetch;
}

describe('account security page', () => {
	it('renders separate controls for signing out locally and everywhere', () => {
		const { body } = render(SecurityPage);

		expect(body).toMatch(/<h1[^>]*>Account security<\/h1>/i);
		expect(body).toMatch(/<button[^>]*>[\s\S]*?Sign out everywhere[\s\S]*?<\/button>/i);
		expect(body).toMatch(/<button[^>]*>[\s\S]*?Sign out[\s\S]*?<\/button>/i);
	});

	it('verifies an authenticated session with a read-only vault request', async () => {
		const requests: Array<{ url: string; method: string }> = [];
		const fetchFn = (async (input: RequestInfo | URL, init?: RequestInit) => {
			requests.push({ url: String(input), method: init?.method ?? 'GET' });
			return new Response('{}', { status: 200 });
		}) as typeof fetch;

		const data = await load({ fetch: fetchFn } as never);

		expect(data?.sessionCheckFailed).toBe(false);
		expect(requests).toHaveLength(1);
		expect(requests[0]).toMatchObject({ url: expect.stringMatching(/\/v1\/vault$/), method: 'GET' });
		expect(requests.some(({ url }) => /refresh|rotate/i.test(url))).toBe(false);
	});

	it('redirects a 401 to login, clears the stale label and library snapshots, and does not rotate the session', async () => {
		const snapshot = JSON.stringify({
			savedAt: new Date(1_000).toISOString(),
			items: [{ id: 'track-1', title: 'Track 1' }]
		});
		const session = memoryStorage({
			'nntune:signed-in-email': 'stale@example.com',
			[LIBRARY_AUTH_CONTEXT_KEY]: 'ctx-1',
			[`${LIBRARY_SNAPSHOT_KEY}.ctx-1`]: snapshot,
			[LIBRARY_SNAPSHOT_KEY]: snapshot
		});
		const local = memoryStorage({
			[LIBRARY_AUTH_CONTEXT_KEY]: 'ctx-1',
			[`${LIBRARY_SNAPSHOT_KEY}.ctx-1`]: snapshot,
			[LIBRARY_SNAPSHOT_KEY]: snapshot
		});
		const broadcasts: string[] = [];
		const watchedLocal: SnapshotStorage = {
			getItem: (key) => local.getItem(key),
			setItem: (key, value) => {
				if (key === LIBRARY_AUTH_TRANSITION_KEY) broadcasts.push(value);
				local.setItem(key, value);
			},
			removeItem: (key) => local.removeItem(key)
		};
		// The label helpers read the bare global; the library cache reads
		// window.* — both must be the same session store, as in a browser.
		vi.stubGlobal('sessionStorage', session);
		vi.stubGlobal('window', { sessionStorage: session, localStorage: watchedLocal });
		const requests: string[] = [];
		const fetchFn = (async (input: RequestInfo | URL) => {
			requests.push(String(input));
			return new Response('{"error":"unauthorized"}', { status: 401 });
		}) as typeof fetch;

		try {
			await expect(load({ fetch: fetchFn } as never)).rejects.toMatchObject({
				status: 307,
				location: '/login'
			});

			expect(session.map.has('nntune:signed-in-email')).toBe(false);
			// The confirmed-ended session must not leave this tab's (or the
			// legacy local) library snapshots readable, and other tabs must
			// be told to drop theirs.
			expect(session.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();
			expect(session.getItem(`${LIBRARY_SNAPSHOT_KEY}.ctx-1`)).toBeNull();
			expect(session.getItem(LIBRARY_SNAPSHOT_KEY)).toBeNull();
			expect(local.getItem(LIBRARY_AUTH_CONTEXT_KEY)).toBeNull();
			expect(local.getItem(`${LIBRARY_SNAPSHOT_KEY}.ctx-1`)).toBeNull();
			expect(local.getItem(LIBRARY_SNAPSHOT_KEY)).toBeNull();
			expect(broadcasts).toHaveLength(1);
			expect(requests).toHaveLength(1);
			expect(requests[0]).toMatch(/\/v1\/vault$/);
			expect(requests.some((url) => /refresh|rotate/i.test(url))).toBe(false);
		} finally {
			vi.unstubAllGlobals();
		}
	});

	it('keeps the page available and announces a non-401 verification failure', async () => {
		const data = await load({ fetch: sessionFetch(503) } as never);
		const { body } = render(SecurityPage, { props: { data } });

		expect(data?.sessionCheckFailed).toBe(true);
		expect(body).toContain('role="status"');
		expect(body).toContain('Could not verify your sign-in status.');
		expect(body).toContain('Sign out everywhere');
		expect(body).toContain('Sign out');
	});
});
