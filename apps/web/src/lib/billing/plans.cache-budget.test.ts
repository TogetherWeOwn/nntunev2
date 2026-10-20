import { describe, expect, it } from 'vitest';
import { getPlan, listPlans, type Plan } from './plans';

/**
 * Acceptance contract: display-only cache budget and retention metadata.
 *
 * Each frontend fallback tier carries the accepted cache allowance as
 * display metadata, separate from cache priority:
 *
 * | Plan    | Cache budget (GiB) | Cache retention (days) | Cache priority |
 * |---------|--------------------|------------------------|----------------|
 * | Free    | 2                  | 7                      | low            |
 * | Plus    | 20                 | 30                     | standard       |
 * | Pro 5x  | 100                | 90                     | high           |
 * | Pro 20x | 400                | 90                     | highest        |
 *
 * GiB means 1,073,741,824 bytes, so the byte budgets are 2,147,483,648;
 * 21,474,836,480; 107,374,182,400; and 429,496,729,600. These are
 * reclaimable cache allowances shown to the reader, not durable storage
 * promises and not a client-side enforcement gate. Prices and the
 * existing quota values do not change.
 */

const GIB_BYTES = 1_073_741_824;

/** The display fields this contract adds to each plan row. */
interface CacheAllowance {
	/** Cache budget in bytes (integer, GiB-denominated). */
	cacheBytesBudget: number;
	/** Cache retention in days. */
	cacheRetentionDays: number;
}

type PlanWithCacheAllowance = Plan & CacheAllowance;

function plansWithCacheAllowance(): PlanWithCacheAllowance[] {
	return listPlans() as PlanWithCacheAllowance[];
}

function planWithCacheAllowance(id: string): PlanWithCacheAllowance | undefined {
	return getPlan(id) as PlanWithCacheAllowance | undefined;
}

const EXPECTED_ALLOWANCE = [
	{ id: 'free', gib: 2, bytes: 2_147_483_648, retentionDays: 7, priority: 'low' },
	{ id: 'plus', gib: 20, bytes: 21_474_836_480, retentionDays: 30, priority: 'standard' },
	{ id: 'pro-5x', gib: 100, bytes: 107_374_182_400, retentionDays: 90, priority: 'high' },
	{ id: 'pro-20x', gib: 400, bytes: 429_496_729_600, retentionDays: 90, priority: 'highest' }
] as const;

describe('plan cache budget and retention metadata', () => {
	it('carries a cache byte budget on all four tiers, in exact GiB bytes', () => {
		for (const expected of EXPECTED_ALLOWANCE) {
			const plan = planWithCacheAllowance(expected.id);
			expect(plan, `plan ${expected.id} exists`).toBeDefined();
			expect(
				plan!.cacheBytesBudget,
				`${expected.id} cacheBytesBudget is ${expected.gib} GiB in bytes`
			).toBe(expected.bytes);
			expect(
				plan!.cacheBytesBudget,
				`${expected.id} cacheBytesBudget uses 1 GiB = ${GIB_BYTES} bytes`
			).toBe(expected.gib * GIB_BYTES);
			expect(
				Number.isInteger(plan!.cacheBytesBudget),
				`${expected.id} cacheBytesBudget is an integer number of bytes`
			).toBe(true);
		}
	});

	it('carries a cache retention in days on all four tiers', () => {
		for (const expected of EXPECTED_ALLOWANCE) {
			const plan = planWithCacheAllowance(expected.id);
			expect(
				plan!.cacheRetentionDays,
				`${expected.id} cacheRetentionDays is ${expected.retentionDays}`
			).toBe(expected.retentionDays);
			expect(
				Number.isInteger(plan!.cacheRetentionDays),
				`${expected.id} cacheRetentionDays is an integer number of days`
			).toBe(true);
		}
	});

	it('keeps budget, retention and priority as three distinct values', () => {
		for (const expected of EXPECTED_ALLOWANCE) {
			const plan = planWithCacheAllowance(expected.id)!;
			expect(plan.cachePriority, `${expected.id} keeps its priority`).toBe(expected.priority);
			// Priority stays its own field: it must not be replaced by, or
			// merged with, the numeric budget/retention values.
			expect(typeof plan.cachePriority).toBe('string');
			expect(typeof plan.cacheBytesBudget).toBe('number');
			expect(typeof plan.cacheRetentionDays).toBe('number');
		}
		// Budgets strictly increase up the ladder; retention is not a
		// restatement of the budget (the top two tiers share 90 days).
		const budgets = plansWithCacheAllowance().map((p) => p.cacheBytesBudget);
		expect(budgets).toEqual([...budgets].sort((a, b) => a - b));
		expect(new Set(budgets).size).toBe(4);
		expect(planWithCacheAllowance('pro-5x')!.cacheRetentionDays).toBe(
			planWithCacheAllowance('pro-20x')!.cacheRetentionDays
		);
	});

	it('exposes the allowance on every listed plan, with no tier missing it', () => {
		const plans = plansWithCacheAllowance();
		expect(plans.map((p) => p.id)).toEqual(['free', 'plus', 'pro-5x', 'pro-20x']);
		for (const plan of plans) {
			expect(
				plan.cacheBytesBudget,
				`${plan.id} listed plan carries cacheBytesBudget`
			).toBeGreaterThan(0);
			expect(
				plan.cacheRetentionDays,
				`${plan.id} listed plan carries cacheRetentionDays`
			).toBeGreaterThan(0);
		}
	});
});

describe('cache allowance does not change prices or existing quotas (controls)', () => {
	it('keeps the accepted prices unchanged', () => {
		expect(getPlan('free')?.priceCents).toBe(0);
		expect(getPlan('plus')?.priceCents).toBe(800);
		expect(getPlan('pro-5x')?.priceCents).toBe(3000);
		expect(getPlan('pro-20x')?.priceCents).toBe(10000);
	});

	it('keeps the accepted stream, concurrency, library and ingest caps unchanged', () => {
		expect(
			listPlans().map((p) => [
				p.streamHoursMonth,
				p.maxConcurrentStreams,
				p.maxLibraryTracks,
				p.ingestJobsDay
			])
		).toEqual([
			[10, 1, 1000, 5],
			[100, 2, 25000, 25],
			[500, 4, 100000, 100],
			[2000, 8, 500000, 400]
		]);
	});
});
