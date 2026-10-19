import { describe, expect, it, vi } from 'vitest';

import { ApiClient, ApiError } from './client.js';
import { BillingApiError, fetchPlans, startCheckout } from '../billing/billingClient.js';
import { onPaywall, type PaywallModel } from '../billing/planLimit.js';

// Protected acceptance spec: JSON body reads in BOTH the API client and the
// billing client are capped at 1 MiB (1_048_576 bytes) for success AND error
// bodies. Today both call `response.json()` uncapped, so a hostile or broken
// upstream can make the browser buffer and parse an arbitrarily large body;
// the per-request deadline bounds time, not bytes.
//
// The contract the Builder implements (one shared capped reader):
// - The cap counts BYTES, not characters, and does not trust Content-Length:
//   a chunked/streamed body with no (or a small) declared length is still cut.
// - Reading stops early: an over-cap stream is not consumed to the end and
//   its reader is cancelled — rejecting only after buffering the whole body
//   is not a cap.
// - A body of exactly 1 MiB is at the cap and parses normally; 1 MiB + 1 byte
//   is over it. Small/fast bodies behave exactly as before.
// - Over-cap surfaces the existing typed error (`ApiError` /
//   `BillingApiError`), never a bare `SyntaxError`. It keeps the real HTTP
//   status, uses the generic `request_failed` code, is NOT retryable (an
//   over-cap body does not become acceptable on retry — including on 5xx,
//   where undecodable bodies otherwise fall back to retryable), carries no
//   paywall and emits no paywall model, and its message never leaks body
//   bytes. For error responses the over-cap body is not parsed at all: no
//   server message is extracted from it, the generic status fallback applies.

const CAP = 1024 * 1024; // 1 MiB
const MARKER = 'BODY-CAP-7f3a';

/** Valid JSON `{"data":"<marker><x…>"}` of exactly `totalBytes` ASCII bytes. */
function jsonBodyOfExactBytes(totalBytes: number): string {
	const prefix = `{"data":"${MARKER}`;
	const suffix = '"}';
	const body = prefix + 'x'.repeat(totalBytes - prefix.length - suffix.length) + suffix;
	expect(new TextEncoder().encode(body).length).toBe(totalBytes);
	return body;
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

/** Valid plans envelope padded (extra fields are allowed) to exact bytes. */
function plansBodyOfExactBytes(totalBytes: number): string {
	const prefix = `{"plans":[${JSON.stringify(PLAN)}],"pad":"${MARKER}`;
	const suffix = '"}';
	const body = prefix + 'x'.repeat(totalBytes - prefix.length - suffix.length) + suffix;
	expect(new TextEncoder().encode(body).length).toBe(totalBytes);
	return body;
}

/** Valid error JSON whose server message must never be read over-cap. */
function errorBodyOfExactBytes(totalBytes: number): string {
	const prefix = `{"error":"boom","pad":"${MARKER}`;
	const suffix = '"}';
	const body = prefix + 'x'.repeat(totalBytes - prefix.length - suffix.length) + suffix;
	expect(new TextEncoder().encode(body).length).toBe(totalBytes);
	return body;
}

function responseWith(body: string, status: number): Response {
	return new Response(body, { status, headers: { 'content-type': 'application/json' } });
}

function clientReturning(response: Response): ApiClient {
	const fetchFn = vi.fn(async () => response);
	return new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });
}

function fetching(response: Response): typeof fetch {
	return vi.fn(async () => response) as unknown as typeof fetch;
}

function expectApiOverCap(failure: unknown, status: number): ApiError {
	expect(failure).toBeInstanceOf(ApiError);
	expect(failure).not.toBeInstanceOf(SyntaxError);
	const apiError = failure as ApiError;
	expect(apiError.name).toBe('ApiError');
	expect(apiError.status).toBe(status);
	expect(apiError.code).toBe('request_failed');
	expect(apiError.retryable).toBe(false);
	expect(apiError.paywall).toBeNull();
	expect(apiError.message.length).toBeGreaterThan(0);
	expect(apiError.message).not.toContain(MARKER);
	expect(apiError.message).not.toContain('Unexpected');
	return apiError;
}

function expectBillingOverCap(failure: unknown, status: number, label: string): BillingApiError {
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
	expect(billingError.message).not.toContain(MARKER);
	expect(billingError.message).not.toContain('Unexpected');
	return billingError;
}

describe('ApiClient JSON body cap (1 MiB)', () => {
	it('rejects a 200 body one byte over the cap instead of resolving it', async () => {
		const client = clientReturning(responseWith(jsonBodyOfExactBytes(CAP + 1), 200));

		const failure = await client.get('/me').catch((error: unknown) => error);

		expectApiOverCap(failure, 200);
	});

	it('rejects an over-cap 201 POST body', async () => {
		const client = clientReturning(responseWith(jsonBodyOfExactBytes(CAP + 4096), 201));

		const failure = await client
			.post('/v1/auth/magic/start', { email: 'alice@example.com' })
			.catch((error: unknown) => error);

		expectApiOverCap(failure, 201);
	});

	it('counts bytes, not characters: a body under 1M chars but over 1 MiB rejects', async () => {
		// 'é' is 2 bytes in UTF-8: ~600k chars is ~1.2 MB on the wire.
		const body = JSON.stringify({ data: MARKER + 'é'.repeat(600_000) });
		expect(body.length).toBeLessThan(CAP);
		expect(new TextEncoder().encode(body).length).toBeGreaterThan(CAP);
		const client = clientReturning(responseWith(body, 200));

		const failure = await client.get('/me').catch((error: unknown) => error);

		expectApiOverCap(failure, 200);
	});

	it('stops reading an over-cap stream early and cancels it', async () => {
		const encoder = new TextEncoder();
		const chunk = encoder.encode('x'.repeat(128 * 1024));
		let pulls = 0;
		let cancelled = false;
		const stream = new ReadableStream<Uint8Array>({
			start(controller) {
				controller.enqueue(encoder.encode(`{"data":"${MARKER}`));
			},
			pull(controller) {
				pulls += 1;
				// ~5 MiB total if fully consumed; valid JSON only when whole.
				if (pulls > 40) {
					controller.enqueue(encoder.encode('"}'));
					controller.close();
				} else {
					controller.enqueue(chunk);
				}
			},
			cancel() {
				cancelled = true;
			}
		});
		const response = new Response(stream, {
			status: 200,
			headers: { 'content-type': 'application/json' }
		});
		const client = clientReturning(response);

		const failure = await client.get('/me').catch((error: unknown) => error);

		expectApiOverCap(failure, 200);
		// 8 x 128 KiB fills the cap; a capped reader pulls only a little past
		// that and cancels, never the full 41 pulls an uncapped read takes.
		expect(pulls).toBeLessThanOrEqual(12);
		expect(cancelled).toBe(true);
	});

	it('rejects an over-cap streamed body that declares a small Content-Length', async () => {
		const body = jsonBodyOfExactBytes(CAP + 8192);
		const response = new Response(body, {
			status: 200,
			headers: { 'content-type': 'application/json', 'content-length': '64' }
		});
		const client = clientReturning(response);

		const failure = await client.get('/me').catch((error: unknown) => error);

		expectApiOverCap(failure, 200);
	});

	it('does not parse an over-cap error body: generic fallback, not retryable', async () => {
		const client = clientReturning(responseWith(errorBodyOfExactBytes(CAP + 1), 500));

		const failure = await client.get('/me').catch((error: unknown) => error);

		const apiError = expectApiOverCap(failure, 500);
		// The server's own "boom" lives in the unparsed body and must not surface.
		expect(apiError.message).toBe('request failed with status 500');
		expect(apiError.message).not.toContain('boom');
	});

	it('emits no paywall model for an over-cap 2xx', async () => {
		const models: PaywallModel[] = [];
		const unsubscribe = onPaywall((model) => models.push(model));
		const client = clientReturning(responseWith(jsonBodyOfExactBytes(CAP + 1), 200));

		const failure = await client.get('/me').catch((error: unknown) => error);
		unsubscribe();

		expectApiOverCap(failure, 200);
		expect(models).toEqual([]);
	});
});

describe('billingClient JSON body cap (1 MiB)', () => {
	it('rejects an over-cap 200 plans body instead of resolving the plans', async () => {
		const failure = await fetchPlans(
			fetching(responseWith(plansBodyOfExactBytes(CAP + 1), 200))
		).catch((error: unknown) => error);

		expectBillingOverCap(failure, 200, 'Load plans');
	});

	it('rejects an over-cap 200 checkout body instead of reading a url from it', async () => {
		const prefix = `{"url":"https://checkout.stripe.com/c/pay_123","pad":"${MARKER}`;
		const suffix = '"}';
		const body = prefix + 'x'.repeat(CAP + 1 - prefix.length - suffix.length) + suffix;
		expect(new TextEncoder().encode(body).length).toBe(CAP + 1);

		const failure = await startCheckout('plus', fetching(responseWith(body, 200))).catch(
			(error: unknown) => error
		);

		expectBillingOverCap(failure, 200, 'Start checkout');
	});

	it('does not parse an over-cap billing error body: generic fallback, not retryable', async () => {
		const failure = await fetchPlans(
			fetching(responseWith(errorBodyOfExactBytes(CAP + 1), 502))
		).catch((error: unknown) => error);

		const billingError = expectBillingOverCap(failure, 502, 'Load plans');
		expect(billingError.message).toBe('Load plans failed (502): request failed with status 502');
		expect(billingError.message).not.toContain('boom');
	});
});

describe('body-cap controls (at/under the cap, adjacent paths unchanged)', () => {
	it('api: a body of exactly 1 MiB still parses', async () => {
		const body = jsonBodyOfExactBytes(CAP);
		const client = clientReturning(responseWith(body, 200));

		await expect(client.get('/me')).resolves.toEqual(JSON.parse(body));
	});

	it('api: a 512 KiB body still parses', async () => {
		const body = jsonBodyOfExactBytes(512 * 1024);
		const client = clientReturning(responseWith(body, 200));

		await expect(client.get('/me')).resolves.toEqual(JSON.parse(body));
	});

	it('api: a small multibyte body still parses', async () => {
		const client = clientReturning(responseWith(JSON.stringify({ data: 'é'.repeat(1000) }), 200));

		await expect(client.get('/me')).resolves.toEqual({ data: 'é'.repeat(1000) });
	});

	it('api: a small valid 200 body is unchanged', async () => {
		const client = clientReturning(responseWith(JSON.stringify({ user: 'ada' }), 200));

		await expect(client.get('/me')).resolves.toEqual({ user: 'ada' });
	});

	it('api: a small JSON error body still yields the server message', async () => {
		const client = clientReturning(responseWith(JSON.stringify({ error: 'boom' }), 500));

		const failure = await client.get('/me').catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).status).toBe(500);
		expect((failure as ApiError).message).toBe('boom');
	});

	it('api: a small non-JSON 502 keeps its retryable status fallback', async () => {
		const response = new Response('<html><body>gateway</body></html>', {
			status: 502,
			headers: { 'content-type': 'text/html' }
		});
		const client = clientReturning(response);

		const failure = await client.get('/me').catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(ApiError);
		const apiError = failure as ApiError;
		expect(apiError.status).toBe(502);
		expect(apiError.code).toBe('request_failed');
		expect(apiError.message).toBe('request failed with status 502');
		expect(apiError.retryable).toBe(true);
	});

	it('billing: a plans body of exactly 1 MiB still resolves the plans', async () => {
		await expect(
			fetchPlans(fetching(responseWith(plansBodyOfExactBytes(CAP), 200)))
		).resolves.toEqual([PLAN]);
	});

	it('billing: a small valid plans body is unchanged', async () => {
		await expect(
			fetchPlans(fetching(responseWith(JSON.stringify({ plans: [PLAN] }), 200)))
		).resolves.toEqual([PLAN]);
	});

	it('billing: a small valid checkout body still resolves the url', async () => {
		await expect(
			startCheckout(
				'plus',
				fetching(
					responseWith(JSON.stringify({ url: 'https://checkout.stripe.com/c/pay_123' }), 200)
				)
			)
		).resolves.toBe('https://checkout.stripe.com/c/pay_123');
	});
});
