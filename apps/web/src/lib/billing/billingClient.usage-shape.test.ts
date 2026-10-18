import { describe, expect, it, vi } from 'vitest';
import { BillingApiError, fetchUsage } from './billingClient';

/**
 * Protected acceptance surface: successful (HTTP 200) GET /v1/me/usage
 * payloads must be validated at the client boundary before any caller
 * evaluates or renders them.
 *
 * Contract under test:
 * - `period` is required and uses the existing `YYYY-MM` shape.
 * - `streamSecondsMonth`, `concurrentNow`, `libraryTracks` and
 *   `ingestToday` are required finite, nonnegative numbers.
 * - Unknown extra fields are allowed; valid zero usage is preserved.
 * - Anything else rejects (so loaders take their existing recoverable
 *   failure path) instead of resolving with malformed or fabricated data.
 * - HTTP error semantics (401/404/402/429) are unchanged.
 */

const VALID = {
	period: '2026-10',
	streamSecondsMonth: 3661,
	concurrentNow: 1,
	libraryTracks: 42,
	ingestToday: 3
};

const NUMERIC_FIELDS = ['streamSecondsMonth', 'concurrentNow', 'libraryTracks', 'ingestToday'] as const;

/** HTTP 200 with a JSON-serialized body, as a real endpoint would send. */
function jsonOk(body: unknown): typeof fetch {
	return vi.fn(async () => new Response(JSON.stringify(body), { status: 200 })) as typeof fetch;
}

/**
 * HTTP 200 whose parsed JSON value is returned verbatim. Needed for
 * NaN/Infinity, which JSON.stringify would silently rewrite to null
 * before the client ever saw them.
 */
function rawOk(body: unknown): typeof fetch {
	return vi.fn(async () => ({
		ok: true,
		status: 200,
		headers: new Headers(),
		json: async () => body
	})) as unknown as typeof fetch;
}

function without(field: string): Record<string, unknown> {
	const copy: Record<string, unknown> = { ...VALID };
	delete copy[field];
	return copy;
}

/** Malformed payloads must never surface as a 401/404/402/429 billing error. */
async function expectMalformedRejection(fetchFn: typeof fetch) {
	const failure = await fetchUsage(fetchFn).catch((e: unknown) => e);
	expect(failure).toBeInstanceOf(Error);
	if (failure instanceof BillingApiError) {
		expect([401, 404, 402, 429]).not.toContain(failure.status);
	}
}

describe('fetchUsage accepts well-formed successful payloads', () => {
	it('resolves a canonical usage payload unchanged', async () => {
		await expect(fetchUsage(jsonOk(VALID))).resolves.toEqual(VALID);
	});

	it('preserves all valid zero-usage values instead of rejecting or dropping them', async () => {
		const zero = {
			period: '2026-01',
			streamSecondsMonth: 0,
			concurrentNow: 0,
			libraryTracks: 0,
			ingestToday: 0
		};
		await expect(fetchUsage(jsonOk(zero))).resolves.toEqual(zero);
	});

	it('allows unknown extra fields alongside the required ones', async () => {
		const withExtra = { ...VALID, futureField: 'anything', another: { nested: true } };
		const result = await fetchUsage(jsonOk(withExtra));
		expect(result).toMatchObject(VALID);
	});

	it('accepts boundary periods and large finite values', async () => {
		const body = {
			period: '1999-12',
			streamSecondsMonth: 90.5,
			concurrentNow: 0,
			libraryTracks: 1_000_000,
			ingestToday: 9999
		};
		await expect(fetchUsage(jsonOk(body))).resolves.toEqual(body);
	});
});

describe('fetchUsage rejects malformed successful payloads', () => {
	it.each([
		['empty object', {}],
		['null', null],
		['array', []],
		['string', 'usage'],
		['number', 42],
		['boolean', true]
	])('rejects a top-level %s body', async (_label, body) => {
		await expectMalformedRejection(jsonOk(body));
	});

	it.each(['period', ...NUMERIC_FIELDS])('rejects a payload missing %s', async (field) => {
		await expectMalformedRejection(jsonOk(without(field)));
	});

	it.each(NUMERIC_FIELDS)('rejects a negative %s', async (field) => {
		await expectMalformedRejection(jsonOk({ ...VALID, [field]: -1 }));
	});

	it.each(NUMERIC_FIELDS)('rejects a numeric string for %s', async (field) => {
		await expectMalformedRejection(jsonOk({ ...VALID, [field]: '10' }));
	});

	it.each(NUMERIC_FIELDS)('rejects null for %s', async (field) => {
		await expectMalformedRejection(jsonOk({ ...VALID, [field]: null }));
	});

	it.each(NUMERIC_FIELDS)('rejects a boolean for %s', async (field) => {
		await expectMalformedRejection(jsonOk({ ...VALID, [field]: true }));
	});

	it.each(NUMERIC_FIELDS)('rejects an object for %s', async (field) => {
		await expectMalformedRejection(jsonOk({ ...VALID, [field]: { value: 1 } }));
	});

	it.each(NUMERIC_FIELDS)('rejects NaN for %s', async (field) => {
		await expectMalformedRejection(rawOk({ ...VALID, [field]: Number.NaN }));
	});

	it.each(NUMERIC_FIELDS)('rejects Infinity for %s', async (field) => {
		await expectMalformedRejection(rawOk({ ...VALID, [field]: Number.POSITIVE_INFINITY }));
	});

	it('rejects -Infinity for a numeric field', async () => {
		await expectMalformedRejection(rawOk({ ...VALID, streamSecondsMonth: Number.NEGATIVE_INFINITY }));
	});

	it.each([
		['empty string', ''],
		['single-digit month', '2026-1'],
		['month 13', '2026-13'],
		['month 00', '2026-00'],
		['slash separator', '2026/10'],
		['full date', '2026-10-01'],
		['leading whitespace', ' 2026-10'],
		['free text', 'October 2026'],
		['number', 202610],
		['null', null]
	])('rejects a period that is %s', async (_label, period) => {
		await expectMalformedRejection(jsonOk({ ...VALID, period }));
	});

	it('does not fabricate zero usage for an empty object', async () => {
		// Resolving `{}` as zeros would hide a backend defect behind a
		// plausible-looking empty library; the payload must reject instead.
		await expect(fetchUsage(jsonOk({}))).rejects.toThrow();
	});
});

describe('fetchUsage preserves HTTP error semantics', () => {
	it.each([401, 404, 402, 429])('still surfaces HTTP %s as a BillingApiError with that status', async (status) => {
		const fetchFn = vi.fn(
			async () => new Response(JSON.stringify({ code: 'some_error', message: 'nope' }), { status })
		) as typeof fetch;
		const failure = await fetchUsage(fetchFn).catch((e: unknown) => e);
		expect(failure).toBeInstanceOf(BillingApiError);
		expect((failure as BillingApiError).status).toBe(status);
	});
});
