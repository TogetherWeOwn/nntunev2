import { describe, expect, it } from 'vitest';
import { load } from './+page';
import type { PageData } from './$types';

/**
 * Regression pin for Paperclip Review finding `billing-loader-429-no-retry`
 * on PR #63 (feat(web): show plan-limit paywall for 402 and 429).
 *
 * The billing loader suppresses the generic outage notice for plan-limit
 * statuses (402/429) because the global paywall modal already surfaces them.
 * A 429 is retryable (the modal even counts down `Retry-After`), so the page
 * must still expose its Retry control (`hadFetchError: true`) without
 * re-adding a duplicate `loadError` notice. A 402 is terminal, so the page
 * must expose neither a Retry control nor a duplicate notice — the modal's
 * "Upgrade your plan" CTA owns that path.
 */
function mockFetch(routes: Record<string, { status: number; body: unknown }>): typeof fetch {
	return (async (input: unknown) => {
		const url = typeof input === 'string' ? input : (input as URL).toString();
		const route = routes[url];
		if (!route) return new Response('not found', { status: 404 });
		return new Response(JSON.stringify(route.body), { status: route.status });
	}) as typeof fetch;
}

const USAGE_OK = {
	period: '2026-10',
	streamSecondsMonth: 3600,
	concurrentNow: 0,
	libraryTracks: 10,
	ingestToday: 0
};

const ACTIVE_SUBSCRIPTION = {
	planId: 'plus',
	status: 'active',
	currentPeriodEnd: null,
	trialEndsAt: null
};

async function loadPage(fetchFn: typeof fetch) {
	const data = await load({ fetch: fetchFn } as never);
	expect(data).toBeDefined();
	return data!;
}

describe('billing loader plan-limit retry (TOG-26983)', () => {
	it('keeps the page Retry control with no duplicate notice on a 429 subscription', async () => {
		const fetchFn = mockFetch({
			'/v1/me/subscription': { status: 429, body: { code: 'rate_limited' } },
			'/v1/me/usage': { status: 200, body: USAGE_OK }
		});

		const data = await loadPage(fetchFn);

		expect(data.planUnknown).toBe(true);
		expect(data.hadFetchError).toBe(true);
		expect(data.loadError).toBeNull();
	});

	it('keeps the page Retry control with no duplicate notice on a 429 usage', async () => {
		const fetchFn = mockFetch({
			'/v1/me/subscription': { status: 200, body: ACTIVE_SUBSCRIPTION },
			'/v1/me/usage': { status: 429, body: { code: 'rate_limited' } }
		});

		const data = await loadPage(fetchFn);

		expect(data.hadFetchError).toBe(true);
		expect(data.loadError).toBeNull();
	});

	it('stays non-retryable with no duplicate notice on a terminal 402 subscription', async () => {
		const fetchFn = mockFetch({
			'/v1/me/subscription': { status: 402, body: { code: 'plan_limit_exceeded' } },
			'/v1/me/usage': { status: 200, body: USAGE_OK }
		});

		const data = await loadPage(fetchFn);

		expect(data.planUnknown).toBe(true);
		expect(data.hadFetchError).toBe(false);
		expect(data.loadError).toBeNull();
	});

	it('stays non-retryable with no duplicate notice on a terminal 402 usage', async () => {
		const fetchFn = mockFetch({
			'/v1/me/subscription': { status: 200, body: ACTIVE_SUBSCRIPTION },
			'/v1/me/usage': { status: 402, body: { code: 'plan_limit_exceeded' } }
		});

		const data = await loadPage(fetchFn);

		expect(data.hadFetchError).toBe(false);
		expect(data.loadError).toBeNull();
	});

	it('renders the Retry button for a 429 subscription with no error alert', async () => {
		const { render } = await import('svelte/server');
		const { default: BillingPage } = await import('./+page.svelte');
		const fetchFn = mockFetch({
			'/v1/me/subscription': { status: 429, body: { code: 'rate_limited' } },
			'/v1/me/usage': { status: 200, body: USAGE_OK }
		});

		const data = await loadPage(fetchFn);
		const { body } = render(BillingPage, { props: { data: data as PageData } });

		expect(body).toContain('Retry');
		expect(body).not.toContain('role="alert"');
	});

	it('renders no Retry button and no error alert for a terminal 402 subscription', async () => {
		const { render } = await import('svelte/server');
		const { default: BillingPage } = await import('./+page.svelte');
		const fetchFn = mockFetch({
			'/v1/me/subscription': { status: 402, body: { code: 'plan_limit_exceeded' } },
			'/v1/me/usage': { status: 200, body: USAGE_OK }
		});

		const data = await loadPage(fetchFn);
		const { body } = render(BillingPage, { props: { data: data as PageData } });

		expect(body).not.toContain('Retry');
		expect(body).not.toContain('role="alert"');
	});
});
