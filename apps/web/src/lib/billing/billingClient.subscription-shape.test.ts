import { describe, expect, it, vi } from 'vitest';
import { BillingApiError, fetchSubscription } from './billingClient';

/**
 * Protected acceptance surface: successful (HTTP 200) GET
 * /v1/me/subscription payloads must be validated at the client boundary
 * before any caller renders plan, status or renewal dates.
 *
 * Contract under test (SubscriptionDTO shape):
 * - Body is a plain object (never null/array/scalar).
 * - `planId` is a required non-empty string.
 * - `status` is one of the eight Stripe statuses the subscriptions row
 *   stores verbatim (ADR 0002 data-model, ADR 0005 billing table):
 *   `trialing`/`active`/`past_due`/`canceled`/`unpaid`/`paused`/
 *   `incomplete`/`incomplete_expired`. Anything else (including the
 *   non-existent local `expired` shortcut, misspellings, non-strings)
 *   rejects instead of rendering a raw unknown status.
 * - `currentPeriodEnd` and `trialEndsAt` are required and are either
 *   null or an ISO-8601 date string naming a real calendar date —
 *   garbage strings, loose strings `Date.parse` accepts, and rolled-over
 *   dates (February 30) reject instead of rendering "Invalid Date" or a
 *   shifted renewal day.
 * - `graceUntil` may be absent; when present it is null or a parseable
 *   date string.
 * - Unknown extra fields are allowed.
 * - HTTP error semantics (401/404) are unchanged.
 */

const VALID = {
	planId: 'plus',
	status: 'active',
	currentPeriodEnd: '2026-11-15T12:30:00Z',
	trialEndsAt: null
};

const DATE_FIELDS = ['currentPeriodEnd', 'trialEndsAt', 'graceUntil'] as const;

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

	it.each(
		[
			'trialing',
			'active',
			'past_due',
			'canceled',
			'unpaid',
			'paused',
			'incomplete',
			'incomplete_expired'
		] as const
	)(
		'accepts status %s',
		async (status) => {
			await expect(fetchSubscription(jsonOk({ ...VALID, status }))).resolves.toMatchObject({
				planId: 'plus',
				status
			});
		}
	);

	it('accepts null renewal and trial dates', async () => {
		const body = { ...VALID, currentPeriodEnd: null, trialEndsAt: null };
		await expect(fetchSubscription(jsonOk(body))).resolves.toEqual(body);
	});

	it('accepts parseable dates in every date field, including graceUntil', async () => {
		const body = {
			planId: 'pro-5x',
			status: 'past_due',
			currentPeriodEnd: '2026-11-15T12:30:00.000Z',
			trialEndsAt: '2026-10-01T00:00:00Z',
			graceUntil: '2026-11-22T12:30:00Z'
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
		const withExtra = { ...VALID, futureField: 'anything', another: { nested: true } };
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
		['expired (no local expired status per ADR 0002)', 'expired'],
		['unknown word', 'banana'],
		['plan id in the status slot', 'free'],
		['uppercase', 'ACTIVE'],
		['mixed case', 'Active'],
		['trailing whitespace', 'active '],
		['empty string', ''],
		['null', null],
		['number', 1],
		['boolean', true],
		['object', { value: 'active' }],
		['array', ['active']]
	])('rejects a status that is %s', async (_label, status) => {
		await expectMalformedRejection(jsonOk({ ...VALID, status }));
	});

	it.each([
		['empty string', ''],
		['garbage text', 'garbage'],
		['free text', 'not a date'],
		['month 13', '2026-13-01T00:00:00Z'],
		['month 00', '2026-00-10T00:00:00Z'],
		['hour 25', '2026-10-20T25:00:00Z'],
		['impossible calendar date (Feb 30 rolls over to March)', '2026-02-30T00:00:00Z'],
		['loose string Date.parse accepts (bare 1)', '1'],
		['dashes only', '--'],
		['time only', 'T00:00:00Z'],
		['the literal string Invalid Date', 'Invalid Date']
	])('rejects an unparseable currentPeriodEnd that is %s', async (_label, currentPeriodEnd) => {
		await expectMalformedRejection(jsonOk({ ...VALID, currentPeriodEnd }));
	});

	it.each(DATE_FIELDS)('rejects an impossible calendar date in %s', async (field) => {
		await expectMalformedRejection(jsonOk({ ...VALID, [field]: '2026-02-30T00:00:00Z' }));
	});

	it.each(DATE_FIELDS)('rejects a garbage date string in %s', async (field) => {
		await expectMalformedRejection(jsonOk({ ...VALID, [field]: 'garbage' }));
	});

	it.each(DATE_FIELDS)('rejects an empty-string date in %s', async (field) => {
		await expectMalformedRejection(jsonOk({ ...VALID, [field]: '' }));
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

	it.each(['planId', 'status', 'currentPeriodEnd', 'trialEndsAt'])(
		'rejects a payload missing %s',
		async (field) => {
			await expectMalformedRejection(jsonOk(without(field)));
		}
	);

	it.each([
		['empty string', ''],
		['null', null],
		['number', 7],
		['boolean', true],
		['object', { id: 'plus' }],
		['array', ['plus']]
	])('rejects a planId that is %s', async (_label, planId) => {
		await expectMalformedRejection(jsonOk({ ...VALID, planId }));
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
