import { describe, expect, it } from 'vitest';
import { load } from './+page';
import type { PageData } from './$types';

/**
 * Protected acceptance surface: a valid subscription plus a malformed
 * successful (HTTP 200) usage payload must land on the billing page's
 * existing recoverable loader path — notice + Retry, no usage scoring,
 * no NaN output, no render failure — and a later valid payload recovers.
 */

const ACTIVE_SUBSCRIPTION = {
	planId: 'plus',
	status: 'active',
	currentPeriodEnd: null,
	trialEndsAt: null
};

const VALID_USAGE = {
	period: '2026-10',
	streamSecondsMonth: 3600,
	concurrentNow: 0,
	libraryTracks: 10,
	ingestToday: 0
};

const ZERO_USAGE = {
	period: '2026-10',
	streamSecondsMonth: 0,
	concurrentNow: 0,
	libraryTracks: 0,
	ingestToday: 0
};

function mockFetch(
	routes: Record<string, { status: number; body: unknown; headers?: HeadersInit }>
): typeof fetch {
	return (async (input: unknown) => {
		const url = typeof input === 'string' ? input : (input as URL).toString();
		const route = routes[url];
		if (!route) return new Response('not found', { status: 404 });
		return new Response(JSON.stringify(route.body), { status: route.status, headers: route.headers });
	}) as typeof fetch;
}

function fetchWithUsage(usageBody: unknown, usageStatus = 200): typeof fetch {
	return mockFetch({
		'/v1/me/subscription': { status: 200, body: ACTIVE_SUBSCRIPTION },
		'/v1/me/usage': { status: usageStatus, body: usageBody }
	});
}

/** The loader never returns void; assert it so svelte-check narrows the type. */
async function loadPage(fetchFn: typeof fetch) {
	const data = await load({ fetch: fetchFn } as never);
	expect(data).toBeDefined();
	return data!;
}

async function renderPage(fetchFn: typeof fetch): Promise<string> {
	const { render } = await import('svelte/server');
	const { default: BillingPage } = await import('./+page.svelte');
	const data = await loadPage(fetchFn);
	const { body } = render(BillingPage, { props: { data: data as PageData } });
	return body;
}

describe('billing loader with malformed successful usage', () => {
	it.each([
		['empty object', {}],
		['null', null],
		['missing libraryTracks', { period: '2026-10', streamSecondsMonth: 0, concurrentNow: 0, ingestToday: 0 }],
		['string streamSecondsMonth', { ...VALID_USAGE, streamSecondsMonth: '3600' }],
		['negative libraryTracks', { ...VALID_USAGE, libraryTracks: -1 }],
		['bad period', { ...VALID_USAGE, period: '2026-13' }]
	])(
		'treats %s as a recoverable usage failure, never as scored usage',
		async (_label, usageBody) => {
			const data = await loadPage(fetchWithUsage(usageBody));

			// The confirmed plan is untouched; only usage failed.
			expect(data.planUnknown).toBe(false);
			expect(data.signedOut).toBe(false);
			// Malformed data reaches neither evaluation nor rendering, and
			// no zero usage is fabricated in its place.
			expect(data.usage).toBeNull();
			expect(data.usageStatus).toBeNull();
			// Existing recoverable path: notice + Retry.
			expect(data.loadError).toMatch(/could not load usage/i);
			expect(data.hadFetchError).toBe(true);
		}
	);

	it('renders the notice and Retry without NaN or a render failure for usage {}', async () => {
		const body = await renderPage(fetchWithUsage({}));

		expect(body).toContain('Could not load usage.');
		expect(body).toContain('Retry');
		expect(body).not.toContain('NaN');
		// No usage section may render from the malformed payload.
		expect(body).not.toContain('Usage for');
	});

	it('renders the notice and Retry without NaN for a wrong-typed libraryTracks', async () => {
		const body = await renderPage(fetchWithUsage({ ...VALID_USAGE, libraryTracks: 'many' }));

		expect(body).toContain('Could not load usage.');
		expect(body).toContain('Retry');
		expect(body).not.toContain('NaN');
		expect(body).not.toContain('Usage for');
	});

	it('recovers on a subsequent load once valid usage arrives', async () => {
		const first = await loadPage(fetchWithUsage({}));
		expect(first.usage).toBeNull();
		expect(first.loadError).toMatch(/could not load usage/i);

		const second = await loadPage(fetchWithUsage(VALID_USAGE));
		expect(second.usage).toEqual(VALID_USAGE);
		expect(second.usageStatus).not.toBeNull();
		expect(second.loadError).toBeNull();
		expect(second.hadFetchError).toBe(false);
	});

	it('preserves valid zero usage: scored, rendered, and error-free', async () => {
		const data = await loadPage(fetchWithUsage(ZERO_USAGE));

		expect(data.usage).toEqual(ZERO_USAGE);
		expect(data.usageStatus).not.toBeNull();
		expect(data.usageStatus?.state).toBe('ok');
		expect(data.usageStatus?.streamFraction).toBe(0);
		expect(data.loadError).toBeNull();
		expect(data.hadFetchError).toBe(false);

		const body = await renderPage(fetchWithUsage(ZERO_USAGE));
		expect(body).toContain('Usage for 2026-10');
		expect(body).not.toContain('NaN');
		expect(body).not.toContain('Retry');
	});

	it('accepts a valid usage payload carrying unknown extra fields', async () => {
		const data = await loadPage(fetchWithUsage({ ...VALID_USAGE, futureField: 1 }));

		expect(data.usage).toMatchObject(VALID_USAGE);
		expect(data.usageStatus).not.toBeNull();
		expect(data.loadError).toBeNull();
	});
});

describe('billing loader keeps existing usage-endpoint status semantics', () => {
	it('keeps a 404 usage response as a fresh account: no failure, no retry', async () => {
		const data = await loadPage(fetchWithUsage({ error: 'not found' }, 404));

		expect(data.usage).toBeNull();
		expect(data.usageStatus).toBeNull();
		expect(data.hadFetchError).toBe(false);
		expect(data.loadError).toBeNull();
	});

	it('keeps a 401 usage response as signed-out, not a usage-shape failure', async () => {
		const data = await loadPage(fetchWithUsage({ error: 'unauthorized' }, 401));

		expect(data.signedOut).toBe(true);
		expect(data.loadError).toMatch(/sign in/i);
		expect(data.loadError).not.toMatch(/could not load usage/i);
	});

	it.each([402, 429])('keeps a %s usage response on the limit path, not the outage notice', async (status) => {
		const data = await loadPage(
			mockFetch({
				'/v1/me/subscription': { status: 200, body: ACTIVE_SUBSCRIPTION },
				'/v1/me/usage': {
					status,
					body: { code: 'plan_limit_exceeded' },
					headers: status === 429 ? { 'Retry-After': '45' } : undefined
				}
			})
		);

		expect(data.usage).toBeNull();
		expect(data.loadError).toBeNull();
		expect(data.hadFetchError).toBe(status === 429);
	});
});
