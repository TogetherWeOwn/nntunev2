/**
 * Acceptance contract: display-only plan resolution must stop confirming
 * expired or missing-deadline trials (ADR 0004: trial caps apply only
 * while `status='trialing' AND now() < trial_end`; `active` on the free
 * price is never entitled).
 *
 * The implementation under contract is `resolveEffectivePlan` in
 * `./plans.ts`, using its existing injectable `now` argument:
 *
 * - A `trialing` subscription is confirmed (`planUnknown: false`) only
 *   when `trialEndsAt` is present, parseable as a date, and strictly
 *   after `now`. At or after the deadline, or with a missing, null,
 *   empty, or unparseable deadline, the result is the existing
 *   unconfirmed state: the free-trial fallback row with
 *   `planUnknown: true` — never the subscription's tier as confirmed.
 * - `active` on the free plan is never confirmed, whatever deadline is
 *   attached: free is a time-boxed trial, not a forever entitled tier.
 * - Controls keep their current behavior: `active` paid tiers are
 *   confirmed regardless of any trial deadline, and `past_due` inside
 *   its grace window keeps its real plan.
 *
 * `SubscriptionRef` must carry the deadline the subscription payload
 * already provides (`trialEndsAt: string | null`, optional on the
 * structural ref so existing callers keep compiling). The intersection
 * helper below lets these tests compile before that field exists; the
 * assertions fail until resolution actually reads it.
 *
 * This is display resolution only. Nothing here licenses client-side
 * access enforcement; the server remains authoritative.
 */
import { describe, expect, it } from 'vitest';
import { getPlan, resolveEffectivePlan, type SubscriptionRef } from './plans';

type TrialRef = SubscriptionRef & { trialEndsAt?: string | null };

function resolve(sub: TrialRef | null, now: Date) {
	return resolveEffectivePlan(sub as SubscriptionRef, now);
}

const DEADLINE = '2026-10-29T12:00:00.000Z';
const BEFORE = new Date('2026-10-29T11:59:59.999Z');
const AT = new Date('2026-10-29T12:00:00.000Z');
const AFTER = new Date('2026-10-29T12:00:00.001Z');

describe('trialing subscriptions confirm only before a present, parseable deadline', () => {
	it('confirms a free trial one millisecond before its deadline', () => {
		const r = resolve({ planId: 'free', status: 'trialing', trialEndsAt: DEADLINE }, BEFORE);
		expect(r.plan.id).toBe('free');
		expect(r.planUnknown).toBe(false);
	});

	it('confirms a paid-tier trial one millisecond before its deadline', () => {
		const r = resolve({ planId: 'pro-5x', status: 'trialing', trialEndsAt: DEADLINE }, BEFORE);
		expect(r.plan.id).toBe('pro-5x');
		expect(r.planUnknown).toBe(false);
	});

	it.each([
		['at the deadline', AT],
		['one millisecond after the deadline', AFTER],
		['long after the deadline', new Date('2027-10-29T12:00:00.000Z')]
	])('does not confirm a free trial %s', (_label, now) => {
		const r = resolve({ planId: 'free', status: 'trialing', trialEndsAt: DEADLINE }, now);
		expect(r.planUnknown).toBe(true);
		expect(r.plan.id).toBe('free');
	});

	it.each([
		['at the deadline', AT],
		['after the deadline', AFTER]
	])('does not confirm a paid-tier trial %s, and falls back to the trial row', (_label, now) => {
		const r = resolve({ planId: 'pro-5x', status: 'trialing', trialEndsAt: DEADLINE }, now);
		expect(r.planUnknown).toBe(true);
		expect(r.plan.id).toBe('free');
	});

	it.each([
		['omitted', undefined],
		['null', null],
		['empty string', ''],
		['unparseable', 'not-a-date'],
		['nonsense date words', 'next Friday-ish']
	])('does not confirm a trial whose deadline is %s', (_label, trialEndsAt) => {
		for (const planId of ['free', 'pro-5x'] as const) {
			const r = resolve({ planId, status: 'trialing', trialEndsAt }, BEFORE);
			expect(r.planUnknown, `planId=${planId}`).toBe(true);
			expect(r.plan.id, `planId=${planId}`).toBe('free');
		}
	});

	it('uses the injected clock, not wall-clock now, at the boundary', () => {
		const sub: TrialRef = { planId: 'free', status: 'trialing', trialEndsAt: DEADLINE };
		// Same subscription, three injected instants: the verdict must be
		// a pure function of the injected time.
		expect(resolve(sub, BEFORE).planUnknown).toBe(false);
		expect(resolve(sub, AT).planUnknown).toBe(true);
		expect(resolve(sub, AFTER).planUnknown).toBe(true);
		// Deterministic: repeating a call cannot flip the verdict.
		expect(resolve(sub, BEFORE)).toEqual(resolve(sub, BEFORE));
		expect(resolve(sub, AT)).toEqual(resolve(sub, AT));
	});

	it('accepts other ISO deadline spellings the payload may carry', () => {
		for (const trialEndsAt of ['2026-10-29T17:30:00+05:30', '2026-10-30']) {
			const r = resolve({ planId: 'free', status: 'trialing', trialEndsAt }, BEFORE);
			expect(r.planUnknown, `trialEndsAt=${trialEndsAt}`).toBe(false);
		}
	});

	it('still refuses an unknown plan id even with a live trial deadline', () => {
		const r = resolve(
			{ planId: 'legacy-2019', status: 'trialing', trialEndsAt: DEADLINE },
			BEFORE
		);
		expect(r.planUnknown).toBe(true);
		expect(r.plan.id).toBe('free');
	});
});

describe('active free is not a forever entitled tier', () => {
	it.each([
		['a future deadline', DEADLINE, BEFORE],
		['a past deadline', DEADLINE, AFTER],
		['a null deadline', null, BEFORE],
		['an omitted deadline', undefined, BEFORE]
	])('does not confirm active free with %s', (_label, trialEndsAt, now) => {
		const r = resolve({ planId: 'free', status: 'active', trialEndsAt }, now);
		expect(r.planUnknown).toBe(true);
		expect(r.plan.id).toBe('free');
	});
});

describe('controls: confirmed states keep their existing display behavior', () => {
	it.each([
		['null', null],
		['omitted', undefined],
		['past', DEADLINE],
		['future', '2099-01-01T00:00:00.000Z']
	])('still confirms an active paid tier with a %s trial deadline', (_label, trialEndsAt) => {
		const r = resolve({ planId: 'plus', status: 'active', trialEndsAt }, AFTER);
		expect(r.plan.id).toBe('plus');
		expect(r.planUnknown).toBe(false);
	});

	it('still confirms the top paid tier while active', () => {
		const r = resolve({ planId: 'pro-20x', status: 'active', trialEndsAt: null }, AFTER);
		expect(r.plan).toEqual(getPlan('pro-20x'));
		expect(r.planUnknown).toBe(false);
	});

	it.each([
		['past', DEADLINE],
		['null', null]
	])(
		'still confirms in-grace past_due on its real plan with a %s trial deadline',
		(_label, trialEndsAt) => {
			const r = resolve(
				{
					planId: 'pro-5x',
					status: 'past_due',
					graceUntil: '2026-11-05T00:00:00.000Z',
					trialEndsAt
				},
				AFTER
			);
			expect(r.plan.id).toBe('pro-5x');
			expect(r.planUnknown).toBe(false);
		}
	);

	it('still treats past-grace past_due as lapsed even with a future trial deadline', () => {
		const r = resolve(
			{
				planId: 'pro-5x',
				status: 'past_due',
				graceUntil: '2026-10-01T00:00:00.000Z',
				trialEndsAt: '2099-01-01T00:00:00.000Z'
			},
			AFTER
		);
		expect(r.planUnknown).toBe(true);
		expect(r.plan.id).toBe('free');
	});

	it.each(['canceled', 'expired', 'incomplete'] as const)(
		'still does not confirm status %s even with a future trial deadline',
		(status) => {
			const r = resolve(
				{ planId: 'pro-5x', status, trialEndsAt: '2099-01-01T00:00:00.000Z' },
				BEFORE
			);
			expect(r.planUnknown).toBe(true);
			expect(r.plan.id).toBe('free');
		}
	);

	it('still falls back unconfirmed when there is no subscription', () => {
		const r = resolve(null, BEFORE);
		expect(r.plan.id).toBe('free');
		expect(r.planUnknown).toBe(true);
	});
});
