import { describe, expect, it, vi } from 'vitest';
import { BillingApiError, fetchPlans, type PlanDTO } from './billingClient';

/**
 * Protected acceptance surface: successful (HTTP 200) GET /v1/plans
 * payloads must be validated at the client boundary before any caller
 * renders pricing from them.
 *
 * Contract under test (envelope + per-row display fields):
 * - The body is an object whose `plans` field is an array.
 * - Every row carries a non-empty string `id`, finite non-negative
 *   `priceCents`/`streamHoursMonth`/`maxConcurrentStreams`, and a
 *   boolean `selectable`.
 * - A valid envelope resolves with that array unchanged, including an
 *   empty array; unknown extra fields are allowed.
 * - Anything else rejects (so loaders take their existing recoverable
 *   failure path) instead of resolving `undefined`, a string, or a
 *   malformed row the pricing page would render as `$NaN/mo`.
 * - HTTP error semantics are unchanged.
 */

const PLUS: PlanDTO = {
	id: 'plus',
	priceCents: 800,
	streamHoursMonth: 100,
	maxConcurrentStreams: 1,
	selectable: true
};

const PRO: PlanDTO = {
	id: 'pro',
	priceCents: 1500,
	streamHoursMonth: 300,
	maxConcurrentStreams: 1,
	selectable: true
};

/** HTTP 200 with a JSON-serialized body, as a real endpoint would send. */
function jsonOk(body: unknown): typeof fetch {
	return vi.fn(async () => new Response(JSON.stringify(body), { status: 200 })) as typeof fetch;
}

/** Malformed payloads must never surface as a 401/404/402/429 billing error. */
async function expectMalformedRejection(fetchFn: typeof fetch) {
	const failure = await fetchPlans(fetchFn).catch((e: unknown) => e);
	expect(failure).toBeInstanceOf(Error);
	if (failure instanceof BillingApiError) {
		expect([401, 404, 402, 429]).not.toContain(failure.status);
	}
}

describe('fetchPlans accepts well-formed successful envelopes', () => {
	it('resolves the plans array from a canonical envelope unchanged', async () => {
		await expect(fetchPlans(jsonOk({ plans: [PLUS] }))).resolves.toEqual([PLUS]);
	});

	it('preserves plan order when the envelope lists multiple plans', async () => {
		await expect(fetchPlans(jsonOk({ plans: [PLUS, PRO] }))).resolves.toEqual([PLUS, PRO]);
	});

	it('resolves an empty plans array instead of rejecting it', async () => {
		await expect(fetchPlans(jsonOk({ plans: [] }))).resolves.toEqual([]);
	});

	it('allows unknown extra envelope fields alongside plans', async () => {
		await expect(
			fetchPlans(jsonOk({ plans: [PLUS], futureField: 'anything', meta: { page: 1 } }))
		).resolves.toEqual([PLUS]);
	});

	it('passes plan entries through verbatim, including unknown extra fields', async () => {
		const withExtra = { ...PLUS, futureField: true };
		await expect(fetchPlans(jsonOk({ plans: [withExtra] }))).resolves.toEqual([withExtra]);
	});
});

describe('fetchPlans rejects malformed successful envelopes', () => {
	it('rejects a top-level array body instead of resolving undefined', async () => {
		await expectMalformedRejection(jsonOk([]));
	});

	it('rejects an empty object body instead of resolving undefined', async () => {
		await expectMalformedRejection(jsonOk({}));
	});

	it('rejects a string plans field instead of resolving the string', async () => {
		await expectMalformedRejection(jsonOk({ plans: 'x' }));
	});

	it.each([
		['null', null],
		['string', 'plans'],
		['number', 42],
		['boolean', true]
	])('rejects a top-level %s body', async (_label, body) => {
		await expectMalformedRejection(jsonOk(body));
	});

	it.each([
		['number', 42],
		['null', null],
		['boolean', true],
		['plain object', { 0: PLUS }],
		['array-like object', { length: 1, 0: PLUS }]
	])('rejects a plans field that is a %s', async (_label, plans) => {
		await expectMalformedRejection(jsonOk({ plans }));
	});

	it('rejects an envelope whose plans live under a different key', async () => {
		await expectMalformedRejection(jsonOk({ items: [PLUS] }));
	});

	it.each([
		['a row missing its price', { id: 'plus', selectable: true }],
		['a row with an empty id', { ...PLUS, id: '' }],
		['a row with a non-string id', { ...PLUS, id: 7 }],
		['a row with a negative price', { ...PLUS, priceCents: -1 }],
		['a row with a string price', { ...PLUS, priceCents: '800' }],
		['a row with NaN hours', { ...PLUS, streamHoursMonth: Number.NaN }],
		['a row missing maxConcurrentStreams', { id: 'plus', priceCents: 800, streamHoursMonth: 100, selectable: true }],
		['a row with a non-boolean selectable', { ...PLUS, selectable: 'yes' }],
		['a null row', null],
		['a string row', 'plus']
	])('rejects %s instead of hydrating it over the fallback', async (_label, row) => {
		await expectMalformedRejection(jsonOk({ plans: [row] }));
	});

	it('rejects the whole payload when one row among good ones is malformed', async () => {
		await expectMalformedRejection(jsonOk({ plans: [PLUS, { id: 'pro', selectable: true }] }));
	});
});

describe('fetchPlans preserves HTTP error semantics', () => {
	it.each([429, 500])(
		'still surfaces HTTP %s as a BillingApiError with that status',
		async (status) => {
			const fetchFn = vi.fn(
				async () => new Response(JSON.stringify({ code: 'some_error', message: 'nope' }), { status })
			) as typeof fetch;
			const failure = await fetchPlans(fetchFn).catch((e: unknown) => e);
			expect(failure).toBeInstanceOf(BillingApiError);
			expect((failure as BillingApiError).status).toBe(status);
		}
	);
});
