import { describe, expect, it, vi } from 'vitest';
import { BillingApiError, fetchSubscription } from './billingClient';

/**
 * Protected acceptance surface: successful (HTTP 200) GET
 * /v1/me/subscription payloads must be validated at the client boundary
 * before any caller renders plan, status or renewal dates.
 *
 * Contract under test (ADR 0002 §4 and ADR 0005 status/entitlement table):
 * - Body is a plain object (never null/array/scalar).
 * - `state` is required and one of `no_subscription`, `active`, `lapsed`.
 * - `planId` is null (no live row) or a non-empty string; absent rejects.
 * - All eight verbatim Stripe statuses are valid, including `unpaid`,
 *   `paused` and `incomplete_expired`. Local `expired`, unknown statuses,
 *   misspellings and non-strings reject; `status` may also be null (no
 *   live row). Validation is not entitlement.
 * - `cancelAtPeriodEnd` is a required boolean.
 * - `currentPeriodEnd` is required and is either
 *   null or real ISO calendar dates / zoned timestamps. Impossible dates,
 *   loose Date.parse strings and malformed times reject, not normalize.
 * - `graceUntil` may be absent; when present it follows the same nullable
 *   ISO-date contract.
 * - Unknown extra fields are allowed.
 * - HTTP error semantics (401/404) are unchanged.
 */

const VALID = {
	state: 'active',
	planId: 'plus',
	status: 'active',
	currentPeriodEnd: '2026-11-15T12:30:00Z',
	graceUntil: null,
	cancelAtPeriodEnd: false
};

const NO_SUBSCRIPTION = {
	state: 'no_subscription',
	planId: null,
	status: null,
	currentPeriodEnd: null,
	graceUntil: null,
	cancelAtPeriodEnd: false
};

const DATE_FIELDS = ['currentPeriodEnd', 'graceUntil'] as const;

/** HTTP 200 with a JSON-serialized body, as a real endpoint would send. */
function jsonOk(body: unknown): typeof fetch {
	return vi.fn(async () => new Response(JSON.stringify(body), { status: 200 })) as typeof fetch;
}

function without(field: string): Record<string, unknown> {
	const copy: Record<string, unknown> = { ...VALID };
	delete copy[field];
	return copy;
}

/** Malformed payloads must never surface as a 401/404 billing error. */
async function expectMalformedRejection(fetchFn: typeof fetch) {
	const failure = await fetchSubscription(fetchFn).catch((e: unknown) => e);
	expect(failure).toBeInstanceOf(Error);
	if (failure instanceof BillingApiError) {
		expect([401, 404]).not.toContain(failure.status);
	}
}

describe('fetchSubscription accepts well-formed successful payloads', () => {
	it('resolves a canonical subscription payload unchanged', async () => {
		await expect(fetchSubscription(jsonOk(VALID))).resolves.toEqual(VALID);
	});

	it.each([
		'trialing',
		'active',
		'past_due',
		'canceled',
		'unpaid',
		'paused',
		'incomplete',
		'incomplete_expired'
	] as const)('accepts verbatim Stripe status %s without changing the plan', async (status) => {
		const body = { ...VALID, status };
		await expect(fetchSubscription(jsonOk(body))).resolves.toEqual(body);
	});

	it('accepts a null renewal date', async () => {
		const body = { ...VALID, currentPeriodEnd: null };
		await expect(fetchSubscription(jsonOk(body))).resolves.toEqual(body);
	});

	it('resolves a no_subscription payload with null planId and status', async () => {
		await expect(fetchSubscription(jsonOk(NO_SUBSCRIPTION))).resolves.toEqual(NO_SUBSCRIPTION);
	});

	it.each(['no_subscription', 'active', 'lapsed'] as const)('accepts state %s', async (state) => {
		const body = { ...VALID, state };
		await expect(fetchSubscription(jsonOk(body))).resolves.toEqual(body);
	});

	it.each([true, false])('accepts cancelAtPeriodEnd %s', async (cancelAtPeriodEnd) => {
		const body = { ...VALID, cancelAtPeriodEnd };
		await expect(fetchSubscription(jsonOk(body))).resolves.toEqual(body);
	});

	describe.each(DATE_FIELDS)('%s ISO date acceptance', (field) => {
		it.each([
			'2026-11-15',
			'2028-02-29',
			'2000-02-29',
			'2026-02-28T23:59:59Z',
			'2028-02-29T12:30:00.123Z',
			// Valid local leap days can fall on a different UTC calendar day.
			'2028-02-29T00:15:00+05:30',
			'2028-02-29T23:45:00-03:30',
			'2026-11-15T12:30:00+00:00'
		])('accepts %s unchanged', async (date) => {
			const body = { ...VALID, [field]: date };
			await expect(fetchSubscription(jsonOk(body))).resolves.toEqual(body);
		});
	});

	it('accepts ISO timestamps in every date field, including graceUntil', async () => {
		const body = {
			state: 'lapsed',
			planId: 'pro',
			status: 'past_due',
			currentPeriodEnd: '2026-11-15T12:30:00.000Z',
			graceUntil: '2026-11-22T12:30:00Z',
			cancelAtPeriodEnd: true
		};
		await expect(fetchSubscription(jsonOk(body))).resolves.toEqual(body);
	});

	it('accepts an absent graceUntil and a null graceUntil', async () => {
		await expect(fetchSubscription(jsonOk(VALID))).resolves.toEqual(VALID);
		await expect(fetchSubscription(jsonOk({ ...VALID, graceUntil: null }))).resolves.toMatchObject(
			VALID
		);
	});

	it('allows unknown extra fields alongside the required ones', async () => {
		const withExtra = {
			...VALID,
			trialEndsAt: null,
			futureField: 'anything',
			another: { nested: true }
		};
		const result = await fetchSubscription(jsonOk(withExtra));
		expect(result).toMatchObject(VALID);
	});
});

describe('fetchSubscription rejects malformed successful payloads', () => {
	it.each([
		['empty object', {}],
		['null', null],
		['array', []],
		['array wrapping a valid payload', [VALID]],
		['string', 'subscription'],
		['number', 42],
		['boolean', true]
	])('rejects a top-level %s body', async (_label, body) => {
		await expectMalformedRejection(jsonOk(body));
	});

	it.each([
		['local expired shortcut (not a Stripe status)', 'expired'],
		['unknown word', 'banana'],
		['plan id in the status slot', 'free'],
		['uppercase', 'ACTIVE'],
		['mixed case', 'Active'],
		['trailing whitespace', 'active '],
		['empty string', ''],
		['number', 1],
		['boolean', true],
		['object', { value: 'active' }],
		['array', ['active']]
	])('rejects a status that is %s', async (_label, status) => {
		await expectMalformedRejection(jsonOk({ ...VALID, status }));
	});

	describe.each(DATE_FIELDS)('%s calendar and time validation', (field) => {
		it.each([
			['empty string', ''],
			['garbage text', 'garbage'],
			['free text', 'not a date'],
			['month 13', '2026-13-01T00:00:00Z'],
			['month 00', '2026-00-10T00:00:00Z'],
			['hour 25', '2026-10-20T25:00:00Z'],
			['dashes only', '--'],
			['time only', 'T00:00:00Z'],
			['the literal string Invalid Date', 'Invalid Date'],
			['February 30 date-only', '2028-02-30'],
			['February 30 timestamp', '2028-02-30T12:30:00Z'],
			['non-leap February 29 date-only', '2026-02-29'],
			['non-leap February 29 timestamp', '2026-02-29T12:30:00+05:30'],
			['non-leap century February 29', '2100-02-29'],
			['April 31', '2026-04-31T12:30:00Z'],
			['day zero', '2026-11-00'],
			['day 32', '2026-11-32'],
			['loose single-digit Date.parse input', '1'],
			['loose slash-separated date', '11/15/2026'],
			['loose English date', 'November 15, 2026'],
			['unpadded ISO-looking date', '2026-2-9'],
			['minute 60', '2026-11-15T12:60:00Z'],
			['second 60', '2026-11-15T12:30:60Z'],
			['missing minute digits', '2026-11-15T12::00Z'],
			['fraction without digits', '2026-11-15T12:30:00.Z'],
			['missing time zone', '2026-11-15T12:30:00'],
			['invalid offset hour', '2026-11-15T12:30:00+25:00'],
			['invalid offset minute', '2026-11-15T12:30:00+05:60']
		])('rejects %s rather than normalizing it', async (_label, date) => {
			await expectMalformedRejection(jsonOk({ ...VALID, [field]: date }));
		});
	});

	it.each(DATE_FIELDS)('rejects an epoch number instead of a date string in %s', async (field) => {
		await expectMalformedRejection(jsonOk({ ...VALID, [field]: 1794745800000 }));
	});

	it.each(DATE_FIELDS)('rejects a boolean instead of a date string in %s', async (field) => {
		await expectMalformedRejection(jsonOk({ ...VALID, [field]: true }));
	});

	it.each(DATE_FIELDS)('rejects an object instead of a date string in %s', async (field) => {
		await expectMalformedRejection(jsonOk({ ...VALID, [field]: { iso: '2026-11-15' } }));
	});

	it.each(DATE_FIELDS)('rejects an array instead of a date string in %s', async (field) => {
		await expectMalformedRejection(jsonOk({ ...VALID, [field]: ['2026-11-15'] }));
	});

	it.each(['state', 'planId', 'status', 'currentPeriodEnd', 'cancelAtPeriodEnd'])(
		'rejects a payload missing %s',
		async (field) => {
			await expectMalformedRejection(jsonOk(without(field)));
		}
	);

	it.each([
		['empty string', ''],
		['number', 7],
		['boolean', true],
		['object', { id: 'plus' }],
		['array', ['plus']]
	])('rejects a planId that is %s', async (_label, planId) => {
		await expectMalformedRejection(jsonOk({ ...VALID, planId }));
	});

	it.each([
		['unknown word', 'banana'],
		['empty string', ''],
		['uppercase', 'ACTIVE'],
		['a Stripe status in the state slot', 'past_due'],
		['null', null],
		['number', 1],
		['object', { value: 'active' }]
	])('rejects a state that is %s', async (_label, state) => {
		await expectMalformedRejection(jsonOk({ ...VALID, state }));
	});

	it.each([
		['string true', 'true'],
		['number 1', 1],
		['number 0', 0],
		['null', null],
		['object', {}],
		['array', [false]]
	])('rejects a cancelAtPeriodEnd that is %s', async (_label, cancelAtPeriodEnd) => {
		await expectMalformedRejection(jsonOk({ ...VALID, cancelAtPeriodEnd }));
	});

	it('does not fabricate a free subscription for an empty object', async () => {
		// Resolving `{}` as some default plan/status would hide a backend
		// defect behind a plausible-looking billing page; reject instead.
		await expect(fetchSubscription(jsonOk({}))).rejects.toThrow();
	});
});

describe('fetchSubscription preserves HTTP error semantics', () => {
	it.each([401, 404])(
		'still surfaces HTTP %s as a BillingApiError with that status',
		async (status) => {
			const fetchFn = vi.fn(
				async () => new Response(JSON.stringify({ code: 'some_error', message: 'nope' }), { status })
			) as typeof fetch;
			const failure = await fetchSubscription(fetchFn).catch((e: unknown) => e);
			expect(failure).toBeInstanceOf(BillingApiError);
			expect((failure as BillingApiError).status).toBe(status);
		}
	);
});
