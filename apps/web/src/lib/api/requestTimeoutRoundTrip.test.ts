import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ApiClient, ApiError } from './client.js';
import { parseErrorBody } from './errors.js';

/**
 * Acceptance: the typed deadline failure `ApiClient.timeoutError()`
 * (status 504, code `request_timeout`, retryable) must survive a
 * round-trip through `parseErrorBody` with its code and retryable flag
 * intact.
 *
 * Today `parseErrorBody` does not recognise `request_timeout`, so the
 * same envelope arriving as a decoded error body degrades to the
 * generic `request_failed`. A timeout the client produced itself and
 * an identical envelope parsed from a body must classify the same way.
 *
 * The envelope under test is taken from a real deadline abort, not
 * hand-mirrored, so this spec pins the producer and the parser to each
 * other. No real sleeps: vitest fake timers drive the deadline.
 */

const STILL_PENDING_MS = 1_000;
const DEADLINE_CAP_MS = 600_000;

interface Tracked {
	settled: boolean;
	failure: unknown;
}

function track(promise: Promise<unknown>): Tracked {
	const tracked: Tracked = { settled: false, failure: undefined };
	void promise.then(
		() => {
			tracked.settled = true;
		},
		(error: unknown) => {
			tracked.settled = true;
			tracked.failure = error;
		}
	);
	return tracked;
}

function neverResolvingFetch() {
	return vi.fn((_url: unknown, init?: RequestInit) => {
		const signal = init?.signal ?? undefined;
		return new Promise<Response>((_resolve, reject) => {
			if (signal?.aborted) {
				reject(signal.reason);
				return;
			}
			signal?.addEventListener('abort', () => reject(signal.reason), { once: true });
		});
	}) as unknown as typeof fetch;
}

async function realTimeoutError(): Promise<ApiError> {
	const fetchFn = neverResolvingFetch();
	const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });
	const tracked = track(client.get('/v1/session/me'));

	await vi.advanceTimersByTimeAsync(STILL_PENDING_MS);
	expect(tracked.settled).toBe(false);

	await vi.advanceTimersByTimeAsync(DEADLINE_CAP_MS);
	expect(tracked.settled).toBe(true);
	expect(tracked.failure).toBeInstanceOf(ApiError);
	return tracked.failure as ApiError;
}

describe('request_timeout round-trip through parseErrorBody', () => {
	beforeEach(() => {
		vi.useFakeTimers();
	});

	afterEach(() => {
		vi.useRealTimers();
	});

	it('pins the deadline error the client itself produces (precondition)', async () => {
		const timeout = await realTimeoutError();

		expect(timeout.status).toBe(504);
		expect(timeout.code).toBe('request_timeout');
		expect(timeout.retryable).toBe(true);
		expect(timeout.paywall).toBeNull();
		expect(timeout.message).toMatch(/timed out|timeout|deadline/i);
	});

	it('preserves code, message and retryable when that exact output is parsed back', async () => {
		const timeout = await realTimeoutError();
		const envelope = {
			code: timeout.code,
			message: timeout.message,
			retryable: timeout.retryable
		};

		expect(parseErrorBody(envelope, timeout.status)).toEqual({
			code: 'request_timeout',
			message: timeout.message,
			retryable: true
		});
	});

	it('preserves a request_timeout envelope end-to-end through ApiClient', async () => {
		const message = 'request timed out after 30000ms deadline';
		const fetchFn = vi.fn(
			async () =>
				new Response(JSON.stringify({ code: 'request_timeout', message, retryable: true }), {
					status: 504,
					headers: { 'content-type': 'application/json' }
				})
		);
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(ApiError);
		const apiError = failure as ApiError;
		expect(apiError.status).toBe(504);
		expect(apiError.code).toBe('request_timeout');
		expect(apiError.message).toBe(message);
		expect(apiError.retryable).toBe(true);
		expect(apiError.paywall).toBeNull();
	});

	it.each([
		['explicit retryable=true', true, true],
		['explicit retryable=false', false, false]
	])('keeps %s intact instead of deriving retry policy for the code', (_label, retryable, expected) => {
		expect(
			parseErrorBody({ code: 'request_timeout', message: 'timed out', retryable }, 504)
		).toEqual({
			code: 'request_timeout',
			message: 'timed out',
			retryable: expected
		});
	});

	it('derives retryable from the 504 status when the envelope omits it', () => {
		expect(parseErrorBody({ code: 'request_timeout', message: 'timed out' }, 504)).toEqual({
			code: 'request_timeout',
			message: 'timed out',
			retryable: true
		});
	});

	it('derives retryable from the 504 status when retryable has the wrong type', () => {
		expect(
			parseErrorBody({ code: 'request_timeout', message: 'timed out', retryable: 'yes' }, 504)
		).toEqual({
			code: 'request_timeout',
			message: 'timed out',
			retryable: true
		});
	});

	it('does not degrade to a paywall, auth or rate-limit classification at 504', () => {
		const parsed = parseErrorBody(
			{ code: 'request_timeout', message: 'timed out', retryable: true },
			504
		);

		expect(parsed.code).toBe('request_timeout');
		expect(parsed.code).not.toBe('request_failed');
		expect(parsed.code).not.toBe('plan_limit_exceeded');
		expect(parsed.code).not.toBe('unauthorized');
		expect(parsed.code).not.toBe('rate_limited');
		expect(parsed.retryable).toBe(true);
	});

	it('still degrades an unrecognised future code at 504 to request_failed (control)', () => {
		expect(
			parseErrorBody(
				{ code: 'future_timeout_code_xyz', message: 'from a newer server', retryable: true },
				504
			)
		).toEqual({
			code: 'request_failed',
			message: 'from a newer server',
			retryable: true
		});
	});
});
