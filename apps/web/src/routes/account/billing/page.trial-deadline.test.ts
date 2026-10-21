/**
 * Acceptance contract: the billing loader/view must stop confirming
 * expired or missing-deadline trials (ADR 0004: trial caps apply only
 * while `status='trialing' AND now() < trial_end`; `active` on the free
 * price is never entitled).
 *
 * For a `trialing` subscription whose `trialEndsAt` is in the past or
 * null, and for an `active` free subscription whatever its deadline:
 *
 * - the plan is unconfirmed (`planUnknown: true`) and usage is not
 *   scored against any plan's caps (`usageStatus: null`), so no usage
 *   alert and no upgrade/checkout action renders;
 * - the subscription record itself is preserved for display, the load
 *   is not reported as a fetch failure (no Retry), and the visitor is
 *   not reported signed out;
 * - the page gives truthful trial recovery guidance: the notice names
 *   the trial and its expired/ended or unconfirmed state (never the
 *   generic "subscription is trialing/active" wording, and never a
 *   confirmed-entitlement claim);
 * - recovery stays on the existing route: the Manage billing (portal)
 *   action renders, no checkout action renders, and loading makes no
 *   checkout/portal calls and creates no second subscription.
 *
 * Controls keep current behavior: a trial before its deadline, an
 * active paid tier (any trial deadline), and in-grace `past_due` stay
 * confirmed with usage scored; an unparseable `trialEndsAt` in the
 * payload keeps failing closed at the client-shape layer with a Retry.
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
	// 50h streamed and 40k tracks: over every free-trial cap, so scoring
	// this usage against an unconfirmed plan would render false alerts.
	streamSecondsMonth: 50 * 3600,
	concurrentNow: 0,
	libraryTracks: 40000,
	ingestToday: 0
};

const DAY = 24 * 3600 * 1000;
const pastDeadline = () => new Date(Date.now() - DAY).toISOString();
const futureDeadline = () => new Date(Date.now() + DAY).toISOString();

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

const TRIAL_NOT_CONFIRMED = /trial[^.]*\b(expired|ended|unconfirmed|not confirmed)\b|\b(expired|ended|unconfirmed)\b[^.]*trial/i;

describe('billing loader with an expired or missing-deadline trial', () => {
	it.each([
		['free trial past its deadline', 'free', 'past'],
		['free trial with a null deadline', 'free', 'null'],
		['paid-tier trial past its deadline', 'pro-5x', 'past'],
		['paid-tier trial with a null deadline', 'pro-5x', 'null']
	] as const)(
		'does not confirm a %s, scores no usage, and guides recovery via Manage billing',
		async (_label, planId, deadlineKind) => {
			const subscription = {
				planId,
				status: 'trialing',
				currentPeriodEnd: null,
				trialEndsAt: deadlineKind === 'past' ? pastDeadline() : null
			};
			const calls: RecordedCall[] = [];
			const data = await loadPage(loaderFetch(subscription, calls));

			// The record loaded fine; it simply does not confirm a plan.
			expect(data.subscription).toEqual(subscription);
			expect(data.planUnknown).toBe(true);
			expect(data.planId).toBe('free');
			expect(data.usageStatus).toBeNull();
			expect(data.hadFetchError).toBe(false);
			expect(data.signedOut).toBe(false);
			expect(data.loadError).not.toBeNull();
			expect(data.loadError).toMatch(TRIAL_NOT_CONFIRMED);
			expect(data.loadError).not.toMatch(/subscription is trialing/i);
			expectReadOnlyBillingCalls(calls);

			const body = await renderPage(data);
			expect(body).toMatch(/confirm your plan/i);
			expect(body).toMatch(TRIAL_NOT_CONFIRMED);
			// Existing recovery route only: portal yes, checkout no, and no
			// outage Retry for a state that is not a fetch failure.
			expect(body).toContain('Manage billing');
			expect(body).not.toContain('Upgrade to');
			expect(body).not.toContain('Retry');
			// Usage is loaded but never scored against the unconfirmed plan.
			expect(body).not.toContain('Usage for 2026-10');
			expect(body).not.toContain('Invalid Date');
		}
	);

	it.each([
		['a future deadline', 'future'],
		['a past deadline', 'past'],
		['a null deadline', 'null']
	] as const)(
		'does not confirm active free with %s as a forever tier',
		async (_label, deadlineKind) => {
			const subscription = {
				planId: 'free',
				status: 'active',
				currentPeriodEnd: null,
				trialEndsAt:
					deadlineKind === 'future'
						? futureDeadline()
						: deadlineKind === 'past'
							? pastDeadline()
							: null
			};
			const calls: RecordedCall[] = [];
			const data = await loadPage(loaderFetch(subscription, calls));

			expect(data.subscription).toEqual(subscription);
			expect(data.planUnknown).toBe(true);
			expect(data.planId).toBe('free');
			expect(data.usageStatus).toBeNull();
			expect(data.hadFetchError).toBe(false);
			expect(data.signedOut).toBe(false);
			expect(data.loadError).not.toBeNull();
			expect(data.loadError).toMatch(
				/(trial|free)[^.]*\b(expired|ended|unconfirmed|not confirmed|pick a plan|choose a plan)\b|\b(expired|ended|unconfirmed)\b[^.]*(trial|free)/i
			);
			expect(data.loadError).not.toMatch(/subscription is active/i);
			expectReadOnlyBillingCalls(calls);

			const body = await renderPage(data);
			expect(body).toMatch(/confirm your plan/i);
			expect(body).toContain('Manage billing');
			expect(body).not.toContain('Upgrade to');
			expect(body).not.toContain('Retry');
			expect(body).not.toContain('Usage for 2026-10');
			expect(body).not.toContain('Invalid Date');
		}
	);
});

describe('billing loader controls around the trial deadline', () => {
	it('still confirms a free trial before its deadline and scores usage against trial caps', async () => {
		const subscription = {
			planId: 'free',
			status: 'trialing',
			currentPeriodEnd: null,
			trialEndsAt: futureDeadline()
		};
		const calls: RecordedCall[] = [];
		const data = await loadPage(loaderFetch(subscription, calls));

		expect(data.subscription).toEqual(subscription);
		expect(data.planUnknown).toBe(false);
		expect(data.planId).toBe('free');
		expect(data.usageStatus).not.toBeNull();
		expect(data.usageStatus?.state).toBe('hard');
		expect(data.usageStatus?.upgradeTo).toBe('plus');
		expect(data.loadError).toBeNull();
		expect(data.hadFetchError).toBe(false);
		expect(data.manageBillingOnly).toBe(false);
		expectReadOnlyBillingCalls(calls);
	});

	it('still confirms a paid-tier trial before its deadline on that tier', async () => {
		const subscription = {
			planId: 'pro-5x',
			status: 'trialing',
			currentPeriodEnd: null,
			trialEndsAt: futureDeadline()
		};
		const calls: RecordedCall[] = [];
		const data = await loadPage(loaderFetch(subscription, calls));

		expect(data.planUnknown).toBe(false);
		expect(data.planId).toBe('pro-5x');
		expect(data.usageStatus).not.toBeNull();
		expect(data.usageStatus?.upgradeTo).toBe('pro-20x');
		expect(data.loadError).toBeNull();
		expect(data.manageBillingOnly).toBe(false);
		expectReadOnlyBillingCalls(calls);
	});

	it.each([
		['a past trial deadline', 'past'],
		['a null trial deadline', 'null'],
		['a future trial deadline', 'future']
	] as const)('still confirms an active paid tier with %s', async (_label, deadlineKind) => {
		const subscription = {
			planId: 'plus',
			status: 'active',
			currentPeriodEnd: null,
			trialEndsAt:
				deadlineKind === 'past'
					? pastDeadline()
					: deadlineKind === 'future'
						? futureDeadline()
						: null
		};
		const calls: RecordedCall[] = [];
		const data = await loadPage(loaderFetch(subscription, calls));

		expect(data.planUnknown).toBe(false);
		expect(data.planId).toBe('plus');
		expect(data.usageStatus).not.toBeNull();
		expect(data.loadError).toBeNull();
		expect(data.manageBillingOnly).toBe(false);
		expectReadOnlyBillingCalls(calls);
	});

	it('still confirms in-grace past_due on its real plan despite a past trial deadline', async () => {
		const subscription = {
			planId: 'pro-5x',
			status: 'past_due',
			currentPeriodEnd: null,
			trialEndsAt: pastDeadline(),
			graceUntil: futureDeadline()
		};
		const calls: RecordedCall[] = [];
		const data = await loadPage(loaderFetch(subscription, calls));

		expect(data.planUnknown).toBe(false);
		expect(data.planId).toBe('pro-5x');
		expect(data.manageBillingOnly).toBe(true);
		expect(data.usageStatus).not.toBeNull();
		expect(data.usageStatus?.upgradeTo).toBeNull();
		expect(data.loadError).toMatch(/payment failed/i);
		expectReadOnlyBillingCalls(calls);
	});

	it('keeps an unparseable trial deadline out of state with a recoverable Retry', async () => {
		const subscription = {
			planId: 'free',
			status: 'trialing',
			currentPeriodEnd: null,
			trialEndsAt: 'not-a-date'
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
