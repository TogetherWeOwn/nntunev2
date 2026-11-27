import { expect, it, vi } from 'vitest';
import { fetchPlans, fetchUsage, openPortal, startCheckout } from './billingClient';
import { onPaywall, toPaywallModel } from './planLimit';
import { getPlan, planFromDTO, resolveEffectivePlan } from './plans';
import { evaluateUsage, formatResetInstant, remainingStreamMinutes } from './usage';
import { markUsageWarningsShown, pendingUsageWarnings, type WarningThreshold } from './usageWarnings';

const plus = getPlan('plus')!;

function json(body: unknown): typeof fetch {
	return async () => Response.json(body);
}

it.each([
	'https://checkout.stripe.com:8443/c/pay_fixture',
	'https://fixture@checkout.stripe.com/c/pay_fixture',
	'https:///checkout.stripe.com/c/pay_fixture',
	'https://billing.stripe.com/p/session_fixture'
])('rejects checkout destinations outside its plain Stripe authority (%s)', async (url) => {
	await expect(startCheckout('plus', json({ url }))).rejects.toMatchObject({
		name: 'BillingApiError',
		status: 200,
		message: 'Start checkout failed (200): Invalid checkout URL',
		retryable: false,
		paywall: null
	});
});

it('uses only the portal origin and disables HTTP redirect following for billing POSTs', async () => {
	const url = 'https://billing.stripe.com:443/p/session_fixture?locale=en';
	const fetchFn = vi.fn<typeof fetch>().mockImplementation(json({ url }));
	await expect(openPortal(fetchFn)).resolves.toBe(url);
	await expect(startCheckout('plus', fetchFn)).rejects.toThrow('Invalid checkout URL');
	await expect(openPortal(json({ url: 'https://checkout.stripe.com/c/pay_fixture' })))
		.rejects.toThrow('Invalid portal URL');
	expect(fetchFn.mock.calls.map(([, init]) => init?.redirect)).toEqual(['error', 'error']);
});

it('propagates a failed billing response read without publishing a paywall', async () => {
	const failure = new TypeError('Fixture body stream disconnected');
	const body = new ReadableStream<Uint8Array>({ start: (stream) => stream.error(failure) });
	const listener = vi.fn();
	const unsubscribe = onPaywall(listener);
	try {
		await expect(openPortal(async () => new Response(body, { status: 429 }))).rejects.toBe(failure);
		expect(listener).not.toHaveBeenCalled();
	} finally {
		unsubscribe();
	}
});

it.each([
	[{ ...plus }, { ...plus, selectable: false }],
	[{ ...plus, priceCents: 800.5 }],
	[{ ...plus, streamHoursMonth: Number.MAX_SAFE_INTEGER }],
	[{ ...plus, maxConcurrentStreams: Number.MAX_SAFE_INTEGER + 1 }]
])('rejects ambiguous plans or inexact plan quantities (%j)', async (...plans) => {
	await expect(fetchPlans(json({ plans }))).rejects.toThrow('Invalid plans payload');
});

it('keeps the reserved plan unselectable and rejects invalid direct hydration', () => {
	expect(planFromDTO({ ...getPlan('free')!, selectable: true })?.selectable).toBe(false);
	expect(planFromDTO({ ...plus, priceCents: Number.NaN })).toBeUndefined();
});

it('does not grant display grace on a rolled-over calendar date', () => {
	const subscription = { planId: 'plus', status: 'past_due', graceUntil: '2026-02-30' };
	const now = new Date('2026-02-28T12:00:00Z');
	expect(resolveEffectivePlan(subscription, now).planUnknown).toBe(true);
	expect(resolveEffectivePlan({ ...subscription, graceUntil: '2026-03-01' }, now).planUnknown).toBe(false);
});

it('requires the usage reset instant to agree with the stated UTC month', async () => {
	const snapshot = { period: '2026-12', streamSecondsMonth: 90.5, resetsAt: '2027-01-01' };
	await expect(fetchUsage(json(snapshot))).resolves.toEqual(snapshot);
	await expect(fetchUsage(json({ ...snapshot, resetsAt: '2027-01-01T00:00:00+01:00' })))
		.rejects.toThrow('Invalid usage payload');
});

it.each([
	'-1', '1.5', '2026-12-01', '5, 10', 'Mon, 30 Feb 2026 00:00:00 GMT',
	'Tue, 01 Dec 2026 00:00:99 GMT', 'Tuesday, 01-Dec-26 00:00:99 GMT', 'Tue Dec  1 00:00:99 2026'
])('omits a countdown for an invalid Retry-After value (%s)', (value) => {
	const headers = new Headers({ 'retry-after': value });
	expect(toPaywallModel({ status: 429, headers })).toMatchObject({ retryAfterSeconds: null, countdownText: null });
});

it.each([
	['concurrent_streams=1, ingest_today=5', undefined],
	['stream_hours_month=100', { code: 'plan_limit_exceeded' }],
	['stream_hours_month=100', { code: 'subscription_required', reason: 'lapsed' }]
] as const)('uses the generic 429 paywall for inconsistent metadata (%s, %j)', (limit, body) => {
	const model = toPaywallModel({ status: 429, headers: new Headers({ 'x-plan-limit': limit }), body });
	expect(model).toMatchObject({ headline: 'Please slow down a little', ctaHref: '/pricing' });
});

it('handles deep malformed paywall metadata without recursive traversal', () => {
	let body: unknown = { used: 90, limit: 100 };
	for (let depth = 0; depth < 12_000; depth++) body = { data: body };
	expect(toPaywallModel({ status: 402, headers: new Headers(), body })?.detail)
		.toContain('Your current usage has reached');
	const headers = new Headers();
	expect(toPaywallModel({ status: 402, headers, body: { data: { used: 100, limit: 100 } } })?.detail)
		.toContain('100 of 100');
});

it.each([
	[plus, Number.NaN], [plus, -1], [{ ...plus, streamHoursMonth: Infinity }, 0]
])('reports no remaining headroom when usage or the cap is invalid', (plan, used) => {
	expect(remainingStreamMinutes(plan, used)).toBe(0);
	expect(evaluateUsage(plan, { streamSecondsMonth: used })).toMatchObject({
		state: 'hard', streamHardCapped: true, streamFraction: 0, upgradeTo: null
	});
});

it('omits reset copy for unzoned or rolled-over dates', () => {
	expect(formatResetInstant('2026-02-30T00:00:00Z')).toBe('');
	expect(formatResetInstant('2026-12-01T00:00:00')).toBe('');
});

it('skips storage for invalid warning periods and fractions and writes only known thresholds', () => {
	const storage = { getItem: vi.fn(() => null), setItem: vi.fn() };
	for (const fraction of [NaN, Infinity, -1]) {
		expect(pendingUsageWarnings('2026-12', fraction, storage)).toEqual([]);
	}
	expect(pendingUsageWarnings('2026-13', 0.95, storage)).toEqual([]);
	expect(storage.getItem).not.toHaveBeenCalled();
	markUsageWarningsShown('2026-13', [80], storage);
	markUsageWarningsShown('2026-12', [99 as WarningThreshold, 95], storage);
	expect(storage.setItem).toHaveBeenCalledExactlyOnceWith('nntune:usage-warning:2026-12:95', '1');
});
