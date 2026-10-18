import { describe, expect, it, vi } from 'vitest';

import {
	BillingApiError,
	fetchPlans,
	fetchSubscription,
	fetchUsage,
	openPortal,
	startCheckout
} from './billingClient';
import { onPaywall, type PaywallModel } from './planLimit';

// Protected acceptance spec: a 2xx response whose body cannot be decoded as
// JSON must surface as the typed `BillingApiError` — never a bare
// `SyntaxError` escaping `readJson`'s success path, and never a downstream
// `TypeError` from reading fields off an undecodable body. The typed error
// keeps the real HTTP status and the caller's label in its message, uses the
// generic documented fallback code (`request_failed`, as for undecodable
// error bodies), is not retryable and carries no paywall. Its message must
// not leak the raw body or the decode failure. Valid-JSON success and the
// existing error path are unchanged. Shape validation of well-formed JSON
// is out of scope.

const HTML_SENTINEL = '<html><body>gateway sentinel SUCCESS-SHAPE-6e2c</body></html>';

function responseWith(body: string | null, status: number, contentType?: string): Response {
	return new Response(body, {
		status,
		headers: contentType ? { 'content-type': contentType } : undefined
	});
}

function fetching(response: Response): typeof fetch {
	return vi.fn(async () => response) as unknown as typeof fetch;
}

function expectTypedSuccessFailure(
	failure: unknown,
	status: number,
	label: string
): BillingApiError {
	expect(failure).toBeInstanceOf(BillingApiError);
	expect(failure).not.toBeInstanceOf(SyntaxError);
	expect(failure).not.toBeInstanceOf(TypeError);
	const billingError = failure as BillingApiError;
	expect(billingError.name).toBe('BillingApiError');
	expect(billingError.status).toBe(status);
	expect(billingError.code).toBe('request_failed');
	expect(billingError.retryable).toBe(false);
	expect(billingError.paywall).toBeNull();
	expect(billingError.message.startsWith(`${label} failed (${status})`)).toBe(true);
	expect(billingError.message).not.toContain('SUCCESS-SHAPE-6e2c');
	expect(billingError.message).not.toContain('<');
	expect(billingError.message).not.toContain('Unexpected');
	return billingError;
}

const PLAN = {
	id: 'plus',
	priceCents: 1200,
	streamHoursMonth: 100,
	maxConcurrentStreams: 2,
	maxLibraryTracks: 5000,
	ingestJobsDay: 20,
	cachePriority: 'standard'
};

describe('billingClient non-JSON success responses', () => {
	it('throws BillingApiError for a 200 with an empty body (fetchPlans)', async () => {
		const failure = await fetchPlans(fetching(responseWith('', 200, 'application/json'))).catch(
			(error: unknown) => error
		);

		expectTypedSuccessFailure(failure, 200, 'Load plans');
	});

	it('throws BillingApiError for a 204 No Content (fetchPlans)', async () => {
		const failure = await fetchPlans(fetching(responseWith(null, 204))).catch(
			(error: unknown) => error
		);

		expectTypedSuccessFailure(failure, 204, 'Load plans');
	});

	it('throws BillingApiError for a 200 HTML body without leaking markup (fetchSubscription)', async () => {
		const failure = await fetchSubscription(
			fetching(responseWith(HTML_SENTINEL, 200, 'text/html'))
		).catch((error: unknown) => error);

		expectTypedSuccessFailure(failure, 200, 'Load subscription');
	});

	it('throws BillingApiError for truncated JSON on a 200 labelled application/json (fetchUsage)', async () => {
		const failure = await fetchUsage(
			fetching(responseWith('{"period":', 200, 'application/json'))
		).catch((error: unknown) => error);

		expectTypedSuccessFailure(failure, 200, 'Load usage');
	});

	it('throws BillingApiError for an empty 200 instead of reading a checkout url from nothing', async () => {
		const failure = await startCheckout(
			'plus',
			fetching(responseWith('', 200, 'application/json'))
		).catch((error: unknown) => error);

		expectTypedSuccessFailure(failure, 200, 'Start checkout');
	});

	it('throws BillingApiError for an HTML 201 instead of reading a portal url from markup', async () => {
		const failure = await openPortal(
			fetching(responseWith(HTML_SENTINEL, 201, 'text/html'))
		).catch((error: unknown) => error);

		expectTypedSuccessFailure(failure, 201, 'Open billing portal');
	});

	it('throws BillingApiError when success decoding rejects, without leaking the reason', async () => {
		// Dual-shape stub: whichever reader the implementation uses, the body
		// is undecodable and its contents must stay out of the error.
		const response = {
			ok: true,
			status: 200,
			headers: new Headers({ 'content-type': 'text/html' }),
			json: async () => {
				throw new SyntaxError('Unexpected token < SUCCESS-SHAPE-6e2c');
			},
			text: async () => HTML_SENTINEL
		} as unknown as Response;

		const failure = await fetchPlans(fetching(response)).catch((error: unknown) => error);

		expectTypedSuccessFailure(failure, 200, 'Load plans');
	});

	it('does not emit a paywall model for a non-JSON 2xx', async () => {
		const models: PaywallModel[] = [];
		const unsubscribe = onPaywall((model) => models.push(model));

		const failure = await fetchPlans(
			fetching(responseWith(HTML_SENTINEL, 200, 'text/html'))
		).catch((error: unknown) => error);
		unsubscribe();

		expectTypedSuccessFailure(failure, 200, 'Load plans');
		expect(models).toEqual([]);
	});
});

describe('billingClient success-shape controls', () => {
	it('still returns the plans for a valid 200 body', async () => {
		await expect(
			fetchPlans(fetching(responseWith(JSON.stringify({ plans: [PLAN] }), 200)))
		).resolves.toEqual([PLAN]);
	});

	it('still returns the checkout url for a valid 200 body', async () => {
		await expect(
			startCheckout(
				'plus',
				fetching(responseWith(JSON.stringify({ url: 'https://checkout.stripe.com/c/pay_123' }), 200))
			)
		).resolves.toBe('https://checkout.stripe.com/c/pay_123');
	});

	it('still returns the portal url for a valid 200 body', async () => {
		await expect(
			openPortal(
				fetching(responseWith(JSON.stringify({ url: 'https://billing.stripe.com/p/session_1' }), 200))
			)
		).resolves.toBe('https://billing.stripe.com/p/session_1');
	});

	it('keeps the status fallback for a non-JSON error response', async () => {
		const failure = await fetchPlans(
			fetching(responseWith(HTML_SENTINEL, 502, 'text/html'))
		).catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(BillingApiError);
		const billingError = failure as BillingApiError;
		expect(billingError.status).toBe(502);
		expect(billingError.code).toBe('request_failed');
		expect(billingError.retryable).toBe(true);
		expect(billingError.message).toBe('Load plans failed (502): request failed with status 502');
	});

	it('keeps the paywall model for a 402 billing failure', async () => {
		const failure = await openPortal(
			fetching(responseWith(JSON.stringify({ code: 'plan_limit_exceeded' }), 402))
		).catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(BillingApiError);
		const billingError = failure as BillingApiError;
		expect(billingError.status).toBe(402);
		expect(billingError.code).toBe('plan_limit_exceeded');
		expect(billingError.paywall?.kind).toBe('terminal');
	});
});
