import { describe, expect, it, vi } from 'vitest';
import { render } from 'svelte/server';
import SecurityPage from './+page.svelte';
import { load } from './+page';

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

	it('redirects a 401 to login, clears the stale label, and does not rotate the session', async () => {
		const values = new Map<string, string>([['nntune:signed-in-email', 'stale@example.com']]);
		vi.stubGlobal('sessionStorage', {
			getItem: (key: string) => values.get(key) ?? null,
			setItem: (key: string, value: string) => values.set(key, value),
			removeItem: (key: string) => values.delete(key)
		});
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

			expect(values.has('nntune:signed-in-email')).toBe(false);
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
