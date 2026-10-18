import { describe, expect, it, vi } from 'vitest';
import { BillingApiError, fetchPlans, type PlanDTO } from './billingClient';

/**
 * Protected acceptance surface: successful (HTTP 200) GET /v1/plans
 * payloads must be validated at the client boundary before any caller
 * renders pricing from them.
 *
 * Contract under test (envelope only; per-plan field validation is a
 * separate slice and deliberately not pinned here):
 * - The body is an object whose `plans` field is an array.
 * - A valid envelope resolves with that array unchanged, including an
 *   empty array; unknown extra fields are allowed.
 * - Anything else rejects (so loaders take their existing recoverable
 *   failure path) instead of resolving `undefined`, a string, or any
 *   other non-array value the pricing page would try to render.
 * - HTTP error semantics are unchanged.
 */

const PLUS: PlanDTO = {
	id: 'plus',
	priceCents: 900,
	streamHoursMonth: 100,
	maxConcurrentStreams: 2,
	maxLibraryTracks: 5000,
	ingestJobsDay: 50,
	cachePriority: 'standard'
};

const PRO: PlanDTO = {
	id: 'pro',
	priceCents: 1900,
	streamHoursMonth: 500,
	maxConcurrentStreams: 5,
	maxLibraryTracks: 25000,
	ingestJobsDay: 250,
	cachePriority: 'priority'
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
