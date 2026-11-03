/**
 * Acceptance contract: the billing loader/view confirms a plan only on
 * the server-derived `state: 'active'` (spec §4 / ADR 0005) and gives
 * truthful, distinct surfaces for the two unentitled states (this file
 * was the trial-deadline contract before the trial was removed — the
 * failure mode it guarded, confirming a plan the server has not
 * confirmed, must stay impossible):
 *
 * - `no_subscription` (never subscribed): plan unconfirmed, usage not
 *   scored, the subscribe notice (browsing/library stay usable — only
 *   playback is gated), no checkout action, and never a fetch failure.
 * - `lapsed` (incl. a legacy stored `trialing`): plan unconfirmed,
 *   usage not scored, recovery stays on the existing route — the
 *   Manage billing (portal) action renders, no checkout renders, and
 *   loading makes no checkout/portal calls and creates no second
 *   subscription.
 * - `active` on the reserved free plan is never a forever tier.
 *
 * Controls keep current behavior: `state: 'active'` paid tiers and
 * in-grace `past_due` (state active, portal-only actions) stay
 * confirmed with usage scored; an unparseable date in the payload keeps
 * failing closed at the client-shape layer with a Retry.
 *
 * Display only: nothing here is billing enforcement, and no new
 * checkout path, subscription creation, or backend policy is in scope.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { load } from './+page';
import type { PageData } from './$types';

afterEach(() => vi.unstubAllGlobals());

const USAGE_HEAVY = {
	period: '2026-10',
	// 50h streamed: half of Plus, so scoring it against an unconfirmed
	// plan would render numbers the page has no right to show.
	streamSecondsMonth: 50 * 3600,
	resetsAt: '2026-11-01T00:00:00Z'
};

const DAY = 24 * 3600 * 1000;
const futureIso = () => new Date(Date.now() + DAY).toISOString();

interface RecordedCall {
	url: string;
	method: string;
}

function loaderFetch(subscriptionBody: unknown, calls: RecordedCall[]): typeof fetch {
	const routes: Record<string, { status: number; body: unknown }> = {
		'/v1/me/subscription': { status: 200, body: subscriptionBody },
		'/v1/me/usage': { status: 200, body: USAGE_HEAVY }
	};
	return (async (input: unknown, init?: { method?: string }) => {
		const url = typeof input === 'string' ? input : (input as URL).toString();
		calls.push({ url, method: init?.method ?? 'GET' });
		const route = routes[url];
		if (!route) return new Response('not found', { status: 404 });
		return new Response(JSON.stringify(route.body), { status: route.status });
	}) as typeof fetch;
}

async function loadPage(fetchFn: typeof fetch) {
	const data = await load({ fetch: fetchFn } as never);
	expect(data).toBeDefined();
	return data!;
}

async function renderPage(data: Awaited<ReturnType<typeof loadPage>>): Promise<string> {
	const { render } = await import('svelte/server');
	const { default: BillingPage } = await import('./+page.svelte');
	return render(BillingPage, { props: { data: data as PageData } }).body;
}

/** Loading must never start a checkout, open the portal, or create anything. */
function expectReadOnlyBillingCalls(calls: RecordedCall[]) {
	expect(calls.map((c) => c.url).sort()).toEqual(['/v1/me/subscription', '/v1/me/usage']);
	for (const call of calls) expect(call.method).toBe('GET');
}

describe('billing loader with no subscription (never subscribed)', () => {
	it('does not confirm a plan, scores no usage, and points at plans', async () => {
		const subscription = {
			state: 'no_subscription',
			planId: null,
			status: null,
			currentPeriodEnd: null,
			graceUntil: null,
			cancelAtPeriodEnd: false
		};
		const calls: RecordedCall[] = [];
		const data = await loadPage(loaderFetch(subscription, calls));

		// The record loaded fine; there simply is no plan to confirm.
		expect(data.subscription).toEqual(subscription);
		expect(data.planUnknown).toBe(true);
		expect(data.planId).toBe('free');
		expect(data.usageStatus).toBeNull();
		expect(data.hadFetchError).toBe(false);
		expect(data.signedOut).toBe(false);
		expect(data.manageBillingOnly).toBe(false);
		expect(data.loadError).toMatch(/don't have a subscription yet/i);
		expect(data.loadError).toMatch(/browsing and your library are fully usable/i);
		expectReadOnlyBillingCalls(calls);

		const body = await renderPage(data);
		expect(body).toMatch(/couldn't confirm an active plan/i);
		expect(body).toContain('Current plan: No plan');
		expect(body).toContain('Compare all plans');
		// The purchase route is the primary action, not a portal button for
		// a billing record that does not exist yet.
		expect(body).toContain('See plans');
		expect(body).not.toContain('Manage billing');
		expect(body).not.toContain('Upgrade to');
		expect(body).not.toContain('Retry');
		// Usage is loaded but never scored against an unconfirmed plan.
		expect(body).not.toContain('Usage for 2026-10');
		expect(body).not.toContain('Invalid Date');
	});
});

describe('billing loader with a lapsed subscription', () => {
	it.each([
		['canceled', 'canceled'],
		['unpaid', 'unpaid'],
		['legacy stored trialing', 'trialing']
	] as const)(
		'does not confirm %s, scores no usage, and guides recovery via Manage billing',
		async (_label, status) => {
			const subscription = {
				state: 'lapsed',
				planId: 'pro',
				status,
				currentPeriodEnd: null,
				graceUntil: null,
				cancelAtPeriodEnd: false
			};
			const calls: RecordedCall[] = [];
			const data = await loadPage(loaderFetch(subscription, calls));

			expect(data.subscription).toEqual(subscription);
			expect(data.planUnknown).toBe(true);
			expect(data.manageBillingOnly).toBe(true);
			expect(data.usageStatus).toBeNull();
			expect(data.hadFetchError).toBe(false);
			expect(data.signedOut).toBe(false);
			expect(data.loadError).toMatch(/manage billing to resolve/i);
			expect(data.loadError).toMatch(/playback resumes/i);
			expectReadOnlyBillingCalls(calls);

			const body = await renderPage(data);
			expect(body).toMatch(/couldn't confirm an active plan/i);
			// Existing recovery route only: portal yes, checkout no, and no
			// outage Retry for a state that is not a fetch failure.
			expect(body).toContain('Manage billing to resolve');
			expect(body).not.toContain('Upgrade to');
			expect(body).not.toContain('Retry');
			expect(body).not.toContain('Usage for 2026-10');
			expect(body).not.toContain('Invalid Date');
		}
	);

	it('does not confirm active free as a forever tier', async () => {
		const subscription = {
			state: 'lapsed',
			planId: 'free',
			status: 'active',
			currentPeriodEnd: null,
			graceUntil: null,
			cancelAtPeriodEnd: false
		};
		const calls: RecordedCall[] = [];
		const data = await loadPage(loaderFetch(subscription, calls));

		expect(data.subscription).toEqual(subscription);
		expect(data.planUnknown).toBe(true);
		expect(data.planId).toBe('free');
		expect(data.usageStatus).toBeNull();
		expect(data.hadFetchError).toBe(false);
		expect(data.signedOut).toBe(false);
		expectReadOnlyBillingCalls(calls);

		const body = await renderPage(data);
		expect(body).not.toContain('Upgrade to');
		expect(body).not.toContain('Usage for 2026-10');
	});
});

describe('billing loader controls around server state', () => {
	it('confirms an active paid tier and scores usage against its caps', async () => {
		const subscription = {
			state: 'active',
			planId: 'plus',
			status: 'active',
			currentPeriodEnd: null,
			graceUntil: null,
			cancelAtPeriodEnd: false
		};
		const calls: RecordedCall[] = [];
		const data = await loadPage(loaderFetch(subscription, calls));

		expect(data.subscription).toEqual(subscription);
		expect(data.planUnknown).toBe(false);
		expect(data.planId).toBe('plus');
		expect(data.usageStatus).not.toBeNull();
		expect(data.usageStatus?.state).toBe('ok');
		expect(data.usageStatus?.streamFraction).toBeCloseTo(0.5);
		expect(data.usageStatus?.upgradeTo).toBe('pro');
		expect(data.loadError).toBeNull();
		expect(data.hadFetchError).toBe(false);
		expect(data.manageBillingOnly).toBe(false);
		expectReadOnlyBillingCalls(calls);
	});

	it('confirms in-grace past_due on its real plan, portal-only, with the payment notice', async () => {
		const subscription = {
			state: 'active',
			planId: 'pro',
			status: 'past_due',
			currentPeriodEnd: null,
			graceUntil: futureIso(),
			cancelAtPeriodEnd: false
		};
		const calls: RecordedCall[] = [];
		const data = await loadPage(loaderFetch(subscription, calls));

		expect(data.planUnknown).toBe(false);
		expect(data.planId).toBe('pro');
		expect(data.manageBillingOnly).toBe(true);
		expect(data.usageStatus).not.toBeNull();
		expect(data.usageStatus?.upgradeTo).toBeNull();
		expect(data.loadError).toMatch(/payment failed/i);
		expect(data.loadError).toMatch(/grace period/i);
		expectReadOnlyBillingCalls(calls);
	});

	it('paid-coverage past_due (graceUntil null) gets payment copy that names no grace period', async () => {
		const subscription = {
			state: 'active',
			planId: 'pro',
			status: 'past_due',
			currentPeriodEnd: futureIso(),
			graceUntil: null,
			cancelAtPeriodEnd: false
		};
		const calls: RecordedCall[] = [];
		const data = await loadPage(loaderFetch(subscription, calls));

		expect(data.planUnknown).toBe(false);
		expect(data.planId).toBe('pro');
		expect(data.manageBillingOnly).toBe(true);
		expect(data.loadError).toMatch(/payment failed/i);
		expect(data.loadError).toMatch(/paid period/i);
		expect(data.loadError).not.toMatch(/grace/i);
		expectReadOnlyBillingCalls(calls);
	});

	it('keeps an unparseable date out of state with a recoverable Retry', async () => {
		const subscription = {
			state: 'active',
			planId: 'plus',
			status: 'active',
			currentPeriodEnd: 'not-a-date',
			graceUntil: null,
			cancelAtPeriodEnd: false
		};
		const calls: RecordedCall[] = [];
		const data = await loadPage(loaderFetch(subscription, calls));

		expect(data.subscription).toBeNull();
		expect(data.planUnknown).toBe(true);
		expect(data.usageStatus).toBeNull();
		expect(data.hadFetchError).toBe(true);
		expect(data.loadError).toMatch(/could not load (?:your )?subscription/i);
		expectReadOnlyBillingCalls(calls);

		const body = await renderPage(data);
		expect(body).toContain('Retry');
		expect(body).not.toContain('Invalid Date');
		expect(body).not.toContain('Upgrade to');
	});
});
