import { describe, expect, it } from 'vitest';
import { load } from './+page';

function mockFetch(routes: Record<string, { status: number; body: unknown }>): typeof fetch {
	return (async (input: unknown) => {
		const url = typeof input === 'string' ? input : (input as URL).toString();
		const route = routes[url];
		if (!route) return new Response('not found', { status: 404 });
		return new Response(JSON.stringify(route.body), { status: route.status });
	}) as typeof fetch;
}

const USAGE_HEAVY = {
	period: '2026-10',
	streamSecondsMonth: 50 * 3600,
	concurrentNow: 0,
	libraryTracks: 40000,
	ingestToday: 0
};

/** The loader never returns void; assert it so svelte-check narrows the type. */
async function loadPage(fetchFn: typeof fetch) {
	const data = await load({ fetch: fetchFn } as never);
	expect(data).toBeDefined();
	return data!;
}

describe('billing loader with an unconfirmed plan', () => {
	it('scores no usage and offers no upgrade when only the subscription call fails', async () => {
		const fetchFn = mockFetch({
			'/v1/me/subscription': { status: 500, body: { error: 'boom' } },
			'/v1/me/usage': { status: 200, body: USAGE_HEAVY }
		});

		const data = await loadPage(fetchFn);

		expect(data.planId).toBe('free');
		expect(data.planUnknown).toBe(true);
		// Real usage (50h) exceeds the trial cap (10h): it must NOT be
		// scored against trial limits, so no alert and no upgrade CTA exist.
		expect(data.usageStatus).toBeNull();
		expect(data.loadError).toMatch(/subscription/i);
		expect(data.hadFetchError).toBe(true);
	});

	it.each(['past_due', 'canceled', 'incomplete'] as const)(
		'sends a %s subscription to the billing portal, never to a new checkout',
		async (status) => {
			const fetchFn = mockFetch({
				'/v1/me/subscription': {
					status: 200,
					body: {
						planId: 'pro-5x',
						status,
						currentPeriodEnd: null,
						trialEndsAt: null
					}
				},
				'/v1/me/usage': { status: 200, body: USAGE_HEAVY }
			});

			const data = await loadPage(fetchFn);

			expect(data.planUnknown).toBe(true);
			expect(data.usageStatus).toBeNull();
			expect(data.manageBillingOnly).toBe(true);
			expect(data.loadError).toMatch(new RegExp(status));
		}
	);

	it('tells a signed-out visitor to sign in instead of reporting an outage', async () => {
		const fetchFn = mockFetch({
			'/v1/me/subscription': { status: 401, body: { error: 'unauthorized' } },
			'/v1/me/usage': { status: 401, body: { error: 'unauthorized' } }
		});

		const data = await loadPage(fetchFn);

		expect(data.signedOut).toBe(true);
		expect(data.usageStatus).toBeNull();
		expect(data.loadError).toMatch(/sign in/i);
		expect(data.loadError).not.toMatch(/not reachable/i);
	});

	it('reports an unreachable backend only when both calls fail without a 401', async () => {
		const fetchFn = mockFetch({
			'/v1/me/subscription': { status: 500, body: { error: 'boom' } },
			'/v1/me/usage': { status: 500, body: { error: 'boom' } }
		});

		const data = await loadPage(fetchFn);

		expect(data.signedOut).toBe(false);
		expect(data.loadError).toMatch(/not reachable/i);
	});

	it('still meters usage and offers an upgrade for a confirmed plan', async () => {
		const fetchFn = mockFetch({
			'/v1/me/subscription': {
				status: 200,
				body: { planId: 'plus', status: 'active', currentPeriodEnd: null, trialEndsAt: null }
			},
			'/v1/me/usage': {
				status: 200,
				body: { period: '2026-10', streamSecondsMonth: 90 * 3600, concurrentNow: 0, libraryTracks: 10, ingestToday: 0 }
			}
		});

		const data = await loadPage(fetchFn);

		expect(data.planUnknown).toBe(false);
		expect(data.usageStatus).not.toBeNull();
		expect(data.usageStatus?.state).toBe('soft');
		expect(data.usageStatus?.upgradeTo).toBe('pro-5x');
		expect(data.manageBillingOnly).toBe(false);
	});
});
