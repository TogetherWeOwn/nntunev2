import { describe, expect, it, vi } from 'vitest';

import { BillingApiError, fetchPlans } from './billingClient';
import { onPaywall } from './planLimit';

const HTML_SENTINEL = '<html><body>gateway sentinel UNDECODABLE-BODY-9c1e</body></html>';

function htmlResponse(body: string, status: number): Response {
	return new Response(body, {
		status,
		headers: { 'content-type': 'text/html' }
	});
}

function rejectingResponse(status: number, reason: string): Response {
	return {
		ok: false,
		status,
		headers: new Headers(),
		json: async () => {
			throw new SyntaxError(reason);
		}
	} as unknown as Response;
}

describe('billingClient non-JSON error bodies', () => {
	it('retains BillingApiError for an HTML gateway body without leaking markup', async () => {
		const fetchFn = vi.fn(async () => htmlResponse(HTML_SENTINEL, 502));

		const failure = await fetchPlans(fetchFn as typeof fetch).catch((e: unknown) => e);

		expect(failure).toBeInstanceOf(BillingApiError);
		expect(failure).not.toBeInstanceOf(SyntaxError);
		const billingError = failure as BillingApiError;
		expect(billingError.status).toBe(502);
		expect(billingError.code).toBe('request_failed');
		expect(billingError.retryable).toBe(true);
		expect(billingError.paywall).toBeNull();
		expect(billingError.message).toBe(
			'Load plans failed (502): request failed with status 502'
		);
		expect(billingError.message).not.toContain('UNDECODABLE-BODY-9c1e');
		expect(billingError.message).not.toContain('<html>');
		expect(billingError.message).not.toContain('Unexpected token');
	});

	it('retains BillingApiError for an empty body without leaking a decode error', async () => {
		const fetchFn = vi.fn(async () => new Response('', { status: 500 }));

		const failure = await fetchPlans(fetchFn as typeof fetch).catch((e: unknown) => e);

		expect(failure).toBeInstanceOf(BillingApiError);
		expect(failure).not.toBeInstanceOf(SyntaxError);
		const billingError = failure as BillingApiError;
		expect(billingError.status).toBe(500);
		expect(billingError.code).toBe('request_failed');
		expect(billingError.retryable).toBe(true);
		expect(billingError.message).toBe(
			'Load plans failed (500): request failed with status 500'
		);
		expect(billingError.message).not.toContain('Unexpected end');
	});

	it('retains BillingApiError when json() rejects without leaking the SyntaxError', async () => {
		const fetchFn = vi.fn(async () =>
			rejectingResponse(503, 'Unexpected token < in JSON at position 0')
		);

		const failure = await fetchPlans(fetchFn as typeof fetch).catch((e: unknown) => e);

		expect(failure).toBeInstanceOf(BillingApiError);
		expect(failure).not.toBeInstanceOf(SyntaxError);
		const billingError = failure as BillingApiError;
		expect(billingError.status).toBe(503);
		expect(billingError.code).toBe('service_unavailable');
		expect(billingError.retryable).toBe(true);
		expect(billingError.message).toBe(
			'Load plans failed (503): request failed with status 503'
		);
		expect(billingError.message).not.toContain('Unexpected token');
	});

	it('derives the 401 code from an HTML body without leaking markup', async () => {
		const fetchFn = vi.fn(async () => htmlResponse(HTML_SENTINEL, 401));

		const failure = await fetchPlans(fetchFn as typeof fetch).catch((e: unknown) => e);

		expect(failure).toBeInstanceOf(BillingApiError);
		const billingError = failure as BillingApiError;
		expect(billingError.code).toBe('unauthorized');
		expect(billingError.retryable).toBe(false);
		expect(billingError.message).toBe(
			'Load plans failed (401): request failed with status 401'
		);
		expect(billingError.message).not.toContain('UNDECODABLE-BODY-9c1e');
	});

	it('keeps the terminal paywall for an HTML 402 without leaking markup', async () => {
		const models: { kind: string; status: number }[] = [];
		const unsubscribe = onPaywall((model) => models.push(model));
		const fetchFn = vi.fn(async () => htmlResponse(HTML_SENTINEL, 402));

		const failure = await fetchPlans(fetchFn as typeof fetch).catch((e: unknown) => e);
		unsubscribe();

		expect(failure).toBeInstanceOf(BillingApiError);
		const billingError = failure as BillingApiError;
		expect(billingError.code).toBe('plan_limit_exceeded');
		expect(billingError.retryable).toBe(false);
		expect(billingError.paywall?.kind).toBe('terminal');
		expect(billingError.paywall?.status).toBe(402);
		expect(billingError.message).toBe(
			'Load plans failed (402): request failed with status 402'
		);
		expect(billingError.message).not.toContain('UNDECODABLE-BODY-9c1e');
		expect(models).toHaveLength(1);
	});

	it('falls back to status for a scalar JSON body without leaking the raw value', async () => {
		const fetchFn = vi.fn(
			async () =>
				new Response(JSON.stringify('oops'), {
					status: 500,
					headers: { 'content-type': 'application/json' }
				})
		);

		const failure = await fetchPlans(fetchFn as typeof fetch).catch((e: unknown) => e);

		expect(failure).toBeInstanceOf(BillingApiError);
		const billingError = failure as BillingApiError;
		expect(billingError.code).toBe('request_failed');
		expect(billingError.retryable).toBe(true);
		expect(billingError.message).toBe(
			'Load plans failed (500): request failed with status 500'
		);
		expect(billingError.message).not.toContain('oops');
	});

	it('ignores wrong-field-type code and retryable on a billing failure', async () => {
		const fetchFn = vi.fn(
			async () =>
				new Response(JSON.stringify({ code: 42, message: 42, retryable: 'yes' }), {
					status: 400,
					headers: { 'content-type': 'application/json' }
				})
		);

		const failure = await fetchPlans(fetchFn as typeof fetch).catch((e: unknown) => e);

		expect(failure).toBeInstanceOf(BillingApiError);
		const billingError = failure as BillingApiError;
		expect(billingError.code).toBe('invalid_request');
		expect(billingError.retryable).toBe(false);
		expect(billingError.message).toBe(
			'Load plans failed (400): request failed with status 400'
		);
	});
});
