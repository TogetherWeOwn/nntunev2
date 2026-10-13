import { describe, expect, it } from 'vitest';
import {
	formatPricePerMonth,
	getPlan,
	isTrialPlan,
	listPlans,
	nextPlan,
	streamCapSeconds
} from './plans';

describe('plan matrix', () => {
	it('exposes the four accepted tiers in order', () => {
		expect(listPlans().map((p) => p.id)).toEqual(['free', 'plus', 'pro-5x', 'pro-20x']);
	});

	it('caps match the accepted matrix', () => {
		expect(getPlan('free')?.streamHoursMonth).toBe(10);
		expect(getPlan('plus')?.streamHoursMonth).toBe(100);
		expect(getPlan('pro-5x')?.streamHoursMonth).toBe(500);
		expect(getPlan('pro-20x')?.streamHoursMonth).toBe(2000);
		expect(getPlan('plus')?.maxConcurrentStreams).toBe(2);
		expect(getPlan('pro-20x')?.maxConcurrentStreams).toBe(8);
	});

	it('marks only free as trial', () => {
		expect(isTrialPlan('free')).toBe(true);
		expect(isTrialPlan('plus')).toBe(false);
	});

	it('formats trial copy distinctly from paid tiers', () => {
		expect(formatPricePerMonth(getPlan('free')!)).toContain('14 days');
		expect(formatPricePerMonth(getPlan('plus')!)).toBe('$8/mo');
		expect(formatPricePerMonth(getPlan('pro-20x')!)).toBe('$100/mo');
	});

	it('converts hours to seconds for ledger comparison', () => {
		expect(streamCapSeconds(getPlan('plus')!)).toBe(100 * 3600);
	});

	it('walks the upgrade ladder and stops at the top', () => {
		expect(nextPlan('free')?.id).toBe('plus');
		expect(nextPlan('plus')?.id).toBe('pro-5x');
		expect(nextPlan('pro-5x')?.id).toBe('pro-20x');
		expect(nextPlan('pro-20x')).toBeUndefined();
	});
});
