import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ApiClient, ApiError } from './client.js';
import { onPaywall, type PaywallModel } from '../billing/planLimit.js';

/**
 * Acceptance: a hung ApiClient request must abort after a deadline and
 * surface the typed ApiError instead of wedging auth/BYOP spinners forever.
 *
 * Contract pinned here (the numeric deadline is the implementation's
 * configured choice):
 * - pending shortly after dispatch (1s), settled within a generous cap
 *   (10 fake minutes) — any sane deadline in between passes;
 * - the injected fetch receives an AbortSignal and that signal is aborted
 *   when the deadline fires (injected fetches model real fetch: they never
 *   settle on their own and reject with the signal's reason on abort);
 * - the rejection is an ApiError with retryable=true, paywall=null, a
 *   non-empty code that is not a paywall/auth classification, and a message
 *   naming the timeout/deadline;
 * - exactly one fetch per request (no automatic retry);
 * - fast/complete requests and existing error/paywall classification are
 *   untouched.
 *
 * Hung-case promises are never awaited unguarded: a settled-flag records
 * completion so a missing deadline fails fast instead of hanging the run.
 * No real sleeps: vitest fake timers drive every wait.
 */

const STILL_PENDING_MS = 1_000;
const DEADLINE_CAP_MS = 600_000;

function jsonResponse(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json' }
	});
}

interface Tracked {
	settled: boolean;
	failure: unknown;
	value: unknown;
}

function track(promise: Promise<unknown>): Tracked {
	const tracked: Tracked = { settled: false, failure: undefined, value: undefined };
	void promise.then(
		(value) => {
			tracked.settled = true;
			tracked.value = value;
		},
		(error: unknown) => {
			tracked.settled = true;
			tracked.failure = error;
		}
	);
	return tracked;
}

/**
 * A fetch that never resolves on its own and, like real fetch, rejects with
 * the signal's reason when the passed AbortSignal aborts. Records every
 * signal it is handed (undefined when the client passes none).
 */
function neverResolvingFetch(signals: (AbortSignal | undefined)[]) {
	return vi.fn((_url: unknown, init?: RequestInit) => {
		const signal = init?.signal ?? undefined;
		signals.push(signal);
		return new Promise<Response>((_resolve, reject) => {
			if (signal?.aborted) {
				reject(signal.reason);
				return;
			}
			signal?.addEventListener('abort', () => reject(signal.reason), { once: true });
		});
	}) as unknown as typeof fetch;
}

function expectDeadlineApiError(failure: unknown): void {
	expect(failure).toBeInstanceOf(ApiError);
	const error = failure as ApiError;
	expect(error.name).toBe('ApiError');
	// A deadline abort is transient: retrying may succeed, and it must never
	// be presented as a paywall or an auth/plan classification.
	expect(error.retryable).toBe(true);
	expect(error.paywall).toBeNull();
	expect(typeof error.code).toBe('string');
	expect(error.code.length).toBeGreaterThan(0);
	expect(error.code).not.toBe('plan_limit_exceeded');
	expect(error.code).not.toBe('rate_limited');
	expect(error.code).not.toBe('unauthorized');
	expect(error.message).toMatch(/timed out|timeout|deadline/i);
}

describe('ApiClient request deadline', () => {
	beforeEach(() => {
		vi.useFakeTimers();
	});

	afterEach(() => {
		vi.useRealTimers();
	});

	it('aborts a never-resolving GET after the deadline and surfaces a typed ApiError', async () => {
		const signals: (AbortSignal | undefined)[] = [];
		const fetchFn = neverResolvingFetch(signals);
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const tracked = track(client.get('/v1/session/me'));

		await vi.advanceTimersByTimeAsync(STILL_PENDING_MS);
		expect(tracked.settled).toBe(false);

		await vi.advanceTimersByTimeAsync(DEADLINE_CAP_MS);
		expect(tracked.settled).toBe(true);
		expectDeadlineApiError(tracked.failure);

		expect(fetchFn).toHaveBeenCalledTimes(1);
		expect(signals[0]).toBeInstanceOf(AbortSignal);
		expect(signals[0]?.aborted).toBe(true);

		// No automatic retry and no second settle long after the deadline.
		await vi.advanceTimersByTimeAsync(DEADLINE_CAP_MS);
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});

	it('aborts a hung POST to the vault test endpoint instead of wedging BYOP', async () => {
		const signals: (AbortSignal | undefined)[] = [];
		const fetchFn = neverResolvingFetch(signals);
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const tracked = track(client.post('/v1/vault/test', { provider: {} }));

		await vi.advanceTimersByTimeAsync(STILL_PENDING_MS);
		expect(tracked.settled).toBe(false);

		await vi.advanceTimersByTimeAsync(DEADLINE_CAP_MS);
		expect(tracked.settled).toBe(true);
		expectDeadlineApiError(tracked.failure);
		expect(fetchFn).toHaveBeenCalledTimes(1);
		expect(signals[0]?.aborted).toBe(true);
	});

	it('passes an AbortSignal to fetch for an ordinary fast request', async () => {
		const signals: (AbortSignal | undefined)[] = [];
		const fetchFn = vi.fn(async (_url: unknown, init?: RequestInit) => {
			signals.push(init?.signal ?? undefined);
			return jsonResponse({ ok: true });
		}) as unknown as typeof fetch;
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		await expect(client.get('/health')).resolves.toEqual({ ok: true });

		expect(signals).toHaveLength(1);
		expect(signals[0]).toBeInstanceOf(AbortSignal);
	});

	it('aborts when the success body stalls after the headers arrive', async () => {
		const signals: (AbortSignal | undefined)[] = [];
		const fetchFn = vi.fn((_url: unknown, init?: RequestInit) => {
			const signal = init?.signal ?? undefined;
			signals.push(signal);
			// Headers arrive immediately; the body read never completes on its
			// own and, like a real response body, rejects when aborted.
			const response = {
				ok: true,
				status: 200,
				headers: new Headers({ 'content-type': 'application/json' }),
				json: () =>
					new Promise<unknown>((_resolve, reject) => {
						if (signal?.aborted) {
							reject(signal.reason);
							return;
						}
						signal?.addEventListener('abort', () => reject(signal.reason), { once: true });
					})
			} as unknown as Response;
			return Promise.resolve(response);
		}) as unknown as typeof fetch;
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const tracked = track(client.get('/v1/session/me'));

		await vi.advanceTimersByTimeAsync(STILL_PENDING_MS);
		expect(tracked.settled).toBe(false);

		await vi.advanceTimersByTimeAsync(DEADLINE_CAP_MS);
		expect(tracked.settled).toBe(true);
		expectDeadlineApiError(tracked.failure);
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});

	it('lets the next request on the same client succeed after a deadline abort', async () => {
		const signals: (AbortSignal | undefined)[] = [];
		let hang = true;
		const fetchFn = vi.fn((_url: unknown, init?: RequestInit) => {
			const signal = init?.signal ?? undefined;
			signals.push(signal);
			if (!hang) return Promise.resolve(jsonResponse({ ok: true }));
			return new Promise<Response>((_resolve, reject) => {
				signal?.addEventListener('abort', () => reject(signal.reason), { once: true });
			});
		}) as unknown as typeof fetch;
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const tracked = track(client.get('/v1/session/me'));
		await vi.advanceTimersByTimeAsync(DEADLINE_CAP_MS);
		expect(tracked.settled).toBe(true);
		expectDeadlineApiError(tracked.failure);

		hang = false;
		await expect(client.get('/health')).resolves.toEqual({ ok: true });
		expect(fetchFn).toHaveBeenCalledTimes(2);
	});

	it('resolves fast requests without waiting for the deadline', async () => {
		const fetchFn = vi.fn(async () => jsonResponse({ user: 'ada' }));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const tracked = track(client.get('/v1/session/me'));
		await vi.advanceTimersByTimeAsync(0);

		expect(tracked.settled).toBe(true);
		expect(tracked.failure).toBeUndefined();
		expect(tracked.value).toEqual({ user: 'ada' });

		// Advancing far past any deadline afterwards must not break the client.
		await vi.advanceTimersByTimeAsync(DEADLINE_CAP_MS);
		await expect(client.get('/health')).resolves.toEqual({ user: 'ada' });
		expect(fetchFn).toHaveBeenCalledTimes(2);
	});

	it('settles an immediately-rejecting fetch promptly, without the deadline', async () => {
		const fetchFn = vi.fn(async () => {
			throw new TypeError('network failure');
		}) as unknown as typeof fetch;
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const tracked = track(client.get('/v1/session/me'));
		await vi.advanceTimersByTimeAsync(0);

		expect(tracked.settled).toBe(true);
		expect(tracked.failure).toBeDefined();
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});

	it('keeps the existing 429 envelope and paywall classification intact', async () => {
		const models: PaywallModel[] = [];
		const unsubscribe = onPaywall((model) => models.push(model));
		const fetchFn = vi.fn(async () =>
			jsonResponse({ code: 'rate_limited', message: 'too many requests', retryable: true }, 429)
		);
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);
		unsubscribe();

		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).status).toBe(429);
		expect((failure as ApiError).code).toBe('rate_limited');
		expect((failure as ApiError).retryable).toBe(true);
		expect((failure as ApiError).paywall?.kind).toBe('retryable');
		expect(models).toHaveLength(1);
	});

	it('keeps the existing 500 error classification intact', async () => {
		const fetchFn = vi.fn(async () => jsonResponse({ error: 'boom' }, 500));
		const client = new ApiClient({ baseUrl: 'https://api.example.com', fetchFn });

		const failure = await client.get('/me').catch((error: unknown) => error);

		expect(failure).toBeInstanceOf(ApiError);
		expect((failure as ApiError).status).toBe(500);
		expect((failure as ApiError).paywall).toBeNull();
	});
});
