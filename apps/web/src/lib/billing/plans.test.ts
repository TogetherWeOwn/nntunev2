import { describe, expect, it } from 'vitest';
import {
	formatPricePerMonth,
	getPlan,
	listPlans,
	listSelectablePlans,
	nextPlan,
	planButtonName,
	planFromDTO,
	resolveEffectivePlan,
	streamCapSeconds
} from './plans';

describe('plan matrix', () => {
	it('exposes the accepted tiers in order (reserved free row first)', () => {
		expect(listPlans().map((p) => p.id)).toEqual(['free', 'plus', 'pro', 'max']);
	});

	it('caps match the accepted matrix (migration 0026 / spec §2.1)', () => {
		expect(getPlan('free')?.streamHoursMonth).toBe(0);
		expect(getPlan('plus')?.streamHoursMonth).toBe(100);
		expect(getPlan('pro')?.streamHoursMonth).toBe(300);
		expect(getPlan('max')?.streamHoursMonth).toBe(750);
		for (const plan of listPlans()) expect(plan.maxConcurrentStreams).toBe(1);
	});

	it('marks only the reserved free row non-selectable', () => {
		expect(getPlan('free')?.selectable).toBe(false);
		expect(listSelectablePlans().map((p) => p.id)).toEqual(['plus', 'pro', 'max']);
	});

	it('Max says unlimited only with the 750-hour figure in the same copy', () => {
		const max = getPlan('max')!;
		expect(max.blurb).toContain('Unlimited');
		expect(max.blurb).toContain('750');
	});

	it('formats prices from plan data', () => {
		expect(formatPricePerMonth(getPlan('free')!)).toBe('$0');
		expect(formatPricePerMonth(getPlan('plus')!)).toBe('$8/mo');
		expect(formatPricePerMonth(getPlan('pro')!)).toBe('$15/mo');
		expect(formatPricePerMonth(getPlan('max')!)).toBe('$25/mo');
	});

	it('converts hours to seconds for ledger comparison', () => {
		expect(streamCapSeconds(getPlan('plus')!)).toBe(100 * 3600);
	});

	it('walks the upgrade ladder and stops at the top', () => {
		expect(nextPlan('free')?.id).toBe('plus');
		expect(nextPlan('plus')?.id).toBe('pro');
		expect(nextPlan('pro')?.id).toBe('max');
		expect(nextPlan('max')).toBeUndefined();
	});
});

describe('planFromDTO', () => {
	it('takes numbers and selectability from plan data, copy from the mirror', () => {
		const merged = planFromDTO({
			id: 'plus',
			priceCents: 900,
			streamHoursMonth: 120,
			maxConcurrentStreams: 1,
			selectable: true
		});
		expect(merged?.priceCents).toBe(900);
		expect(merged?.streamHoursMonth).toBe(120);
		expect(merged?.name).toBe('Plus');
		expect(merged?.blurb).toBe(getPlan('plus')?.blurb);
	});

	it('returns undefined for a plan id the client has no copy for', () => {
		expect(
			planFromDTO({
				id: 'ultra',
				priceCents: 9900,
				streamHoursMonth: 9999,
				maxConcurrentStreams: 1,
				selectable: true
			})
		).toBeUndefined();
	});

	it('never makes the reserved free row selectable from data alone', () => {
		const merged = planFromDTO({
			id: 'free',
			priceCents: 0,
			streamHoursMonth: 0,
			maxConcurrentStreams: 1,
			selectable: false
		});
		expect(merged?.selectable).toBe(false);
	});
});

describe('planButtonName', () => {
	it('names each selectable plan distinctly when idle', () => {
		const names = listSelectablePlans().map((p) => planButtonName(p, null));
		expect(names).toEqual(['Choose Plus', 'Choose Pro', 'Choose Max']);
		expect(new Set(names).size).toBe(names.length);
	});

	it('keeps the pending plan distinct from the idle ones', () => {
		expect(planButtonName(getPlan('plus')!, 'plus')).toBe('Starting Plus…');
		expect(planButtonName(getPlan('pro')!, 'plus')).toBe('Choose Pro');
		expect(planButtonName(getPlan('max')!, 'max')).toBe('Starting Max…');
	});
});

describe('resolveEffectivePlan', () => {
	it('uses the subscription plan when the server state is active', () => {
		expect(
			resolveEffectivePlan({ planId: 'plus', status: 'active', state: 'active' })
		).toEqual({ plan: getPlan('plus'), planUnknown: false });
	});

	it('never confirms a stored trialing status (no trial product state)', () => {
		const r = resolveEffectivePlan({ planId: 'pro', status: 'trialing', state: 'lapsed' });
		expect(r.plan.id).toBe('free');
		expect(r.planUnknown).toBe(true);
	});

	it('falls back to the reserved row when there is no subscription', () => {
		const r = resolveEffectivePlan(null);
		expect(r.plan.id).toBe('free');
		expect(r.planUnknown).toBe(true);
	});

	it('falls back to the reserved row on an unknown plan id', () => {
		const r = resolveEffectivePlan({ planId: 'legacy-2019', status: 'active', state: 'active' });
		expect(r.plan.id).toBe('free');
		expect(r.planUnknown).toBe(true);
	});

	it.each(['past_due', 'canceled', 'expired', 'incomplete'] as const)(
		'falls back to the reserved row when status is %s (stateless payload)',
		(status) => {
			const r = resolveEffectivePlan({ planId: 'pro', status });
			expect(r.plan.id).toBe('free');
			expect(r.planUnknown).toBe(true);
		}
	);

	it('keeps the real plan for past_due inside the grace window (stateless payload)', () => {
		const now = new Date('2026-10-15T12:00:00Z');
		const r = resolveEffectivePlan(
			{ planId: 'pro', status: 'past_due', graceUntil: '2026-10-20T00:00:00Z' },
			now
		);
		expect(r.plan.id).toBe('pro');
		expect(r.planUnknown).toBe(false);
	});

	it('treats past_due past the grace window (or with none) as lapsed', () => {
		const now = new Date('2026-10-15T12:00:00Z');
		for (const sub of [
			{ planId: 'pro', status: 'past_due', graceUntil: '2026-10-10T00:00:00Z' },
			{ planId: 'pro', status: 'past_due', graceUntil: null },
			{ planId: 'pro', status: 'past_due' }
		] as const) {
			const r = resolveEffectivePlan(sub, now);
			expect(r.plan.id).toBe('free');
			expect(r.planUnknown).toBe(true);
		}
	});
});
