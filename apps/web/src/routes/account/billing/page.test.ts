import { afterEach, describe, expect, it, vi } from 'vitest';
import { onPaywall, type PaywallModel } from '#lib/billing/planLimit.js';
import { load } from './+page';
import type { PageData } from './$types';

afterEach(() => vi.unstubAllGlobals());

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
	it.each([402, 429])('delivers a cold-load %s limit to the late layout subscriber', async (status) => {
		vi.stubGlobal('window', {});
		const fetchFn = mockFetch({
			'/v1/me/subscription': { status, body: { code: 'plan_limit_exceeded' } },
			'/v1/me/usage': { status: 200, body: USAGE_HEAVY }
		});

		const data = await loadPage(fetchFn);
		const seen: PaywallModel[] = [];
		const unsubscribe = onPaywall((model) => seen.push(model));

		expect(data.hadFetchError).toBe(status === 429);
		expect(data.loadError).toBeNull();
		expect(data.planUnknown).toBe(true);
		expect(seen).toHaveLength(1);
		expect(seen[0]?.status).toBe(status);
		expect(seen[0]?.ctaHref).toBe('/pricing');
		unsubscribe();
	});

	it.each([402, 429])('delivers a usage-endpoint %s limit to the late layout subscriber', async (status) => {
		vi.stubGlobal('window', {});
		const fetchFn = mockFetch({
			'/v1/me/subscription': {
				status: 200,
				body: { planId: 'plus', status: 'active', currentPeriodEnd: null, trialEndsAt: null }
			},
			'/v1/me/usage': {
				status,
				body: { code: 'plan_limit_exceeded' },
				headers: status === 429 ? { 'Retry-After': '45' } : undefined
			}
		});

		const data = await loadPage(fetchFn);
		const seen: PaywallModel[] = [];
		const unsubscribe = onPaywall((model) => seen.push(model));

		expect(data.hadFetchError).toBe(status === 429);
		expect(data.loadError).toBeNull();
		expect(seen).toHaveLength(1);
		expect(seen[0]?.status).toBe(status);
		expect(seen[0]?.ctaHref).toBe('/pricing');
		if (status === 429) expect(seen[0]?.retryAfterSeconds).toBe(45);
		unsubscribe();
	});

	it('replays only the latest limit when both requests fail before layout subscription', async () => {
		vi.stubGlobal('window', {});
		const fetchFn = mockFetch({
			'/v1/me/subscription': { status: 402, body: { code: 'subscription_limit' } },
			'/v1/me/usage': {
				status: 429,
				body: { code: 'rate_limited' },
				headers: { 'Retry-After': '45' }
			}
		});

		await loadPage(fetchFn);
		const seen: PaywallModel[] = [];
		const unsubscribe = onPaywall((model) => seen.push(model));

		expect(seen).toHaveLength(1);
		expect(seen[0]?.status).toBe(429);
		expect(seen[0]?.code).toBe('rate_limited');
		expect(seen[0]?.retryAfterSeconds).toBe(45);
		unsubscribe();
	});

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

	it('treats a 404 subscription as a backend failure with a retry, not a fresh account', async () => {
		const fetchFn = mockFetch({
			'/v1/me/subscription': { status: 404, body: { error: 'not found' } },
			'/v1/me/usage': { status: 200, body: USAGE_HEAVY }
		});

		const data = await loadPage(fetchFn);

		expect(data.planUnknown).toBe(true);
		expect(data.usageStatus).toBeNull();
		expect(data.hadFetchError).toBe(true);
		expect(data.loadError).toMatch(/subscription/i);
	});

	it('treats a 404 usage as a fresh account: no failure, no retry', async () => {
		const fetchFn = mockFetch({
			'/v1/me/subscription': {
				status: 200,
				body: { planId: 'plus', status: 'active', currentPeriodEnd: null, trialEndsAt: null }
			},
			'/v1/me/usage': { status: 404, body: { error: 'not found' } }
		});

		const data = await loadPage(fetchFn);

		expect(data.planUnknown).toBe(false);
		expect(data.usage).toBeNull();
		expect(data.usageStatus).toBeNull();
		expect(data.hadFetchError).toBe(false);
	});

	it('keeps an in-grace past_due subscriber on their own plan with usage scored', async () => {
		const fetchFn = mockFetch({
			'/v1/me/subscription': {
				status: 200,
				body: {
					planId: 'pro-5x',
					status: 'past_due',
					currentPeriodEnd: null,
					trialEndsAt: null,
					graceUntil: new Date(Date.now() + 24 * 3600 * 1000).toISOString()
				}
			},
			'/v1/me/usage': { status: 200, body: USAGE_HEAVY }
		});

		const data = await loadPage(fetchFn);

		expect(data.planUnknown).toBe(false);
		expect(data.planId).toBe('pro-5x');
		expect(data.manageBillingOnly).toBe(true);
		expect(data.usageStatus).not.toBeNull();
		// Portal-only: usage stays scored, but no checkout target may leak
		// through, and the failed payment gets an explicit notice.
		expect(data.usageStatus?.upgradeTo).toBeNull();
		expect(data.loadError).toMatch(/payment failed/i);
	});

	it('renders Manage billing as the only action for an in-grace past_due subscriber', async () => {
		const { render } = await import('svelte/server');
		const { default: BillingPage } = await import('./+page.svelte');
		const fetchFn = mockFetch({
			'/v1/me/subscription': {
				status: 200,
				body: {
					planId: 'pro-5x',
					status: 'past_due',
					currentPeriodEnd: null,
					trialEndsAt: null,
					graceUntil: new Date(Date.now() + 24 * 3600 * 1000).toISOString()
				}
			},
			'/v1/me/usage': { status: 200, body: USAGE_HEAVY }
		});

		const data = await loadPage(fetchFn);
		const { body } = render(BillingPage, { props: { data: data as PageData } });

		expect(body).toContain('Manage billing to resolve');
		expect(body).not.toContain('Upgrade to');
	});

	it.each(['past_due', 'canceled', 'unpaid', 'paused', 'incomplete', 'incomplete_expired'] as const)(
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
