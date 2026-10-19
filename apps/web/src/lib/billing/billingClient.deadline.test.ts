import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
	BillingApiError,
	fetchPlans,
	fetchSubscription,
	fetchUsage,
	openPortal,
	signupUrlForCheckoutFailure,
	startCheckout
} from './billingClient';
import { onPaywall, type PaywallModel } from './planLimit';

/**
 * Protected acceptance spec: every billing helper owns one cancellable lifetime,
 * from dispatch through JSON consumption, including error bodies. A deadline is
 * retryable, never an auth/paywall outcome, and does not resubmit a POST.
 *
 * The duration is the Builder's choice, not an exact constant pinned by this
 * suite. Ten fake minutes is only an observation ceiling (as in ApiClient's
 * timeout acceptance tests). The shared-lifetime tests measure the observable
 * header deadline, then deliver headers halfway through that same budget.
 *
 * All transport is injected. Real Response/ReadableStream bodies model pending
 * reads and cancellation; late-header peers deliberately ignore abort. No hung
 * promise is awaited unguarded and no timer advances in real time.
 */
const OBSERVATION_CAP_MS = 600_000;
const CHECKOUT_URL = 'https://checkout.stripe.com/c/pay_deadline_fixture';
const PORTAL_URL = 'https://billing.stripe.com/p/deadline_fixture';
const PLAN = {
	id: 'plus',
	priceCents: 1200,
	streamHoursMonth: 100,
	maxConcurrentStreams: 2,
	maxLibraryTracks: 5000,
	ingestJobsDay: 20,
	cachePriority: 'standard'
};
const USAGE = {
	period: '2026-10',
	streamSecondsMonth: 3661,
	concurrentNow: 1,
	libraryTracks: 42,
	ingestToday: 3
};
const SUBSCRIPTION = {
	planId: 'plus',
	status: 'active',
	currentPeriodEnd: '2026-11-01T00:00:00Z',
	trialEndsAt: null,
	graceUntil: null
};

interface Operation {
	name: string;
	call: (fetchFn: typeof fetch) => Promise<unknown>;
	body: unknown;
	value: unknown;
	method: string;
}

const OPERATIONS: Operation[] = [
	{ name: 'plans', call: fetchPlans, body: { plans: [PLAN] }, value: [PLAN], method: 'GET' },
	{ name: 'usage', call: fetchUsage, body: USAGE, value: USAGE, method: 'GET' },
	{
		name: 'subscription',
		call: fetchSubscription,
		body: SUBSCRIPTION,
		value: SUBSCRIPTION,
		method: 'GET'
	},
	{
		name: 'checkout',
		call: (fetchFn) => startCheckout('plus', fetchFn),
		body: { url: CHECKOUT_URL },
		value: CHECKOUT_URL,
		method: 'POST'
	},
	{
		name: 'portal',
		call: openPortal,
		body: { url: PORTAL_URL },
		value: PORTAL_URL,
		method: 'POST'
	}
];

function jsonResponse(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json' }
	});
}

function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((done) => {
		resolve = done;
	});
	return { promise, resolve };
}

function track(promise: Promise<unknown>) {
	const state = { settled: false, value: undefined as unknown, failure: undefined as unknown };
	void promise.then(
		(value) => {
			state.settled = true;
			state.value = value;
		},
		(failure: unknown) => {
			state.settled = true;
			state.failure = failure;
		}
	);
	return state;
}

const cleanup: (() => void)[] = [];

function hungHeaders(signals: (AbortSignal | undefined)[]) {
	return vi.fn((_input: unknown, init?: RequestInit) => {
		const signal = init?.signal ?? undefined;
		signals.push(signal);
		return new Promise<Response>((_resolve, reject) => {
			if (signal?.aborted) {
				reject(signal.reason);
				return;
			}
			const abort = () => reject(signal?.reason);
			signal?.addEventListener('abort', abort, { once: true });
			cleanup.push(() => signal?.removeEventListener('abort', abort));
		});
	});
}

function stalledBody(status: number, signal?: AbortSignal) {
	let controller!: ReadableStreamDefaultController<Uint8Array>;
	let released = false;
	const detach = () => signal?.removeEventListener('abort', abort);
	const abort = () => {
		if (released) return;
		released = true;
		detach();
		controller.error(signal?.reason);
	};
	const stream = new ReadableStream<Uint8Array>({
		start(value) {
			controller = value;
			controller.enqueue(new TextEncoder().encode('{"pending":'));
		},
		cancel() {
			released = true;
			detach();
		}
	});
	if (signal?.aborted) abort();
	else signal?.addEventListener('abort', abort, { once: true });
	cleanup.push(() => {
		detach();
		if (!released) {
			released = true;
			controller.error(new Error('fixture cleanup'));
		}
	});
	return {
		response: new Response(stream, { status, headers: { 'content-type': 'application/json' } }),
		released: () => released
	};
}

function expectDeadline(state: ReturnType<typeof track>) {
	expect(state.settled, 'billing operation must settle within the bounded request lifetime').toBe(true);
	expect(state.failure).toBeInstanceOf(BillingApiError);
	const failure = state.failure as BillingApiError;
	expect(failure.name).toBe('BillingApiError');
	expect(failure.retryable).toBe(true);
	expect(failure.paywall).toBeNull();
	expect(failure.code).toEqual(expect.any(String));
	expect(failure.code.length).toBeGreaterThan(0);
	expect(['unauthorized', 'plan_limit_exceeded', 'rate_limited']).not.toContain(failure.code);
	expect([401, 402, 403, 404, 429]).not.toContain(failure.status);
	expect(failure.message).toMatch(/timed out|timeout|deadline/i);
	expect(signupUrlForCheckoutFailure('plus', failure)).toBeNull();
}

// Observe listener retention without replacing AbortSignal behavior. Once-only
// listeners are released by dispatch; otherwise completion must detach them.
function auditCompletion() {
	const added = vi.spyOn(AbortSignal.prototype, 'addEventListener');
	const removed = vi.spyOn(AbortSignal.prototype, 'removeEventListener');
	return () => {
		expect(vi.getTimerCount(), 'completed requests must not retain deadline timers').toBe(0);
		added.mock.calls.forEach(([event, listener, options], index) => {
			if (event !== 'abort') return;
			const signal = added.mock.contexts[index];
			if (typeof options === 'object' && options?.once && signal instanceof AbortSignal && signal.aborted) return;
			expect(removed.mock.calls.some(([removedEvent, removedListener], removalIndex) =>
				removedEvent === 'abort' && removedListener === listener &&
				removed.mock.contexts[removalIndex] === signal
			), 'completed requests must detach abort listeners').toBe(true);
		});
	};
}

let models: PaywallModel[];
beforeEach(() => {
	vi.useFakeTimers();
	models = [];
	cleanup.push(onPaywall((model) => models.push(model)));
});

afterEach(async () => {
	for (const dispose of cleanup.splice(0)) dispose();
	await vi.advanceTimersByTimeAsync(0);
	vi.clearAllTimers();
	vi.restoreAllMocks();
	vi.useRealTimers();
});

describe.each(OPERATIONS)('billing request lifetime: $name', (operation) => {
	it('aborts hung headers with a retryable typed deadline, without automatic resubmission', async () => {
		const assertClean = auditCompletion();
		const signals: (AbortSignal | undefined)[] = [];
		const fetchFn = hungHeaders(signals);
		const state = track(operation.call(fetchFn as typeof fetch));
		await vi.advanceTimersByTimeAsync(0);
		expect(state.settled).toBe(false);

		await vi.advanceTimersByTimeAsync(OBSERVATION_CAP_MS);
		expectDeadline(state);
		expect(signals[0]).toBeInstanceOf(AbortSignal);
		expect(signals[0]?.aborted).toBe(true);
		assertClean();
		expect(models).toEqual([]);
		await vi.advanceTimersByTimeAsync(OBSERVATION_CAP_MS);
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});

	it.each([200, 401, 402, 429, 500])('includes stalled JSON consumption at HTTP %i in the deadline', async (status) => {
		const assertClean = auditCompletion();
		let signal: AbortSignal | undefined;
		let body: ReturnType<typeof stalledBody> | undefined;
		const fetchFn = vi.fn(async (_input: unknown, init?: RequestInit) => {
			signal = init?.signal ?? undefined;
			body = stalledBody(status, signal);
			return body.response;
		});
		const state = track(operation.call(fetchFn as typeof fetch));
		await vi.advanceTimersByTimeAsync(0);
		expect(body?.response.bodyUsed, 'exercise a pending body read, not hung headers').toBe(true);
		expect(state.settled).toBe(false);

		await vi.advanceTimersByTimeAsync(OBSERVATION_CAP_MS);
		expectDeadline(state);
		expect(signal).toBeInstanceOf(AbortSignal);
		expect(signal?.aborted).toBe(true);
		expect(body?.released(), 'pending response body must be released on expiry').toBe(true);
		assertClean();
		expect(models).toEqual([]);
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});

	it('does not restart the lifetime when delayed headers finally arrive', async () => {
		const assertClean = auditCompletion();
		const probe = track(operation.call(hungHeaders([]) as typeof fetch));
		const probeStart = Date.now();
		await vi.advanceTimersByTimeAsync(0);
		expect(vi.getTimerCount(), 'dispatch must arm a cancellable deadline').toBeGreaterThan(0);
		await vi.advanceTimersToNextTimerAsync();
		const budget = Date.now() - probeStart;
		expectDeadline(probe);
		expect(budget).toBeGreaterThan(0);
		expect(budget).toBeLessThanOrEqual(OBSERVATION_CAP_MS);

		const headers = deferred<Response>();
		let signal: AbortSignal | undefined;
		const fetchFn = vi.fn((_input: unknown, init?: RequestInit) => {
			signal = init?.signal ?? undefined;
			return headers.promise;
		});
		const state = track(operation.call(fetchFn as typeof fetch));
		const halfway = Math.floor(budget / 2);
		await vi.advanceTimersByTimeAsync(halfway);
		const body = stalledBody(200, signal);
		headers.resolve(body.response);
		await vi.advanceTimersByTimeAsync(0);
		expect(body.response.bodyUsed).toBe(true);
		expect(state.settled).toBe(false);

		await vi.advanceTimersByTimeAsync(budget - halfway + 1);
		expectDeadline(state);
		expect(signal?.aborted).toBe(true);
		expect(body.released()).toBe(true);
		expect(fetchFn).toHaveBeenCalledTimes(1);
		assertClean();
	});

	it.each([
		{ status: 200, cancelBehavior: 'never settles' },
		{ status: 200, cancelBehavior: 'rejects' },
		{ status: 402, cancelBehavior: 'never settles' },
		{ status: 402, cancelBehavior: 'rejects' }
	])('discards late HTTP $status headers even when body cancellation $cancelBehavior', async ({ status, cancelBehavior }) => {
		const assertClean = auditCompletion();
		const headers = deferred<Response>();
		let signal: AbortSignal | undefined;
		const fetchFn = vi.fn((_input: unknown, init?: RequestInit) => {
			signal = init?.signal ?? undefined;
			// This peer deliberately ignores abort and may return after expiry.
			return headers.promise;
		});
		const state = track(operation.call(fetchFn as typeof fetch));
		await vi.advanceTimersByTimeAsync(OBSERVATION_CAP_MS);
		expectDeadline(state);
		expect(signal?.aborted).toBe(true);
		const failure = state.failure;

		const cancel = vi.fn(() => cancelBehavior === 'never settles'
			? new Promise<void>(() => {})
			: Promise.reject(new Error('late cleanup sentinel')));
		const late = new Response(new ReadableStream<Uint8Array>({ cancel }), { status });
		headers.resolve(late);
		await vi.advanceTimersByTimeAsync(0);
		expect(cancel, 'unread late response must be released without awaiting cleanup').toHaveBeenCalledTimes(1);
		expect(state.failure).toBe(failure);
		expect(state.value).toBeUndefined();
		expect(models).toEqual([]);
		assertClean();
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});

	it('allows an explicit subsequent attempt to succeed with a fresh request lifetime', async () => {
		const assertClean = auditCompletion();
		const signals: (AbortSignal | undefined)[] = [];
		const hanging = hungHeaders(signals);
		const fetchFn = vi.fn((input: unknown, init?: RequestInit) => {
			if (signals.length === 0) return hanging(input, init);
			signals.push(init?.signal ?? undefined);
			return Promise.resolve(jsonResponse(operation.body));
		});
		const first = track(operation.call(fetchFn as typeof fetch));
		await vi.advanceTimersByTimeAsync(OBSERVATION_CAP_MS);
		expectDeadline(first);
		expect(fetchFn).toHaveBeenCalledTimes(1);

		const retry = track(operation.call(fetchFn as typeof fetch));
		await vi.advanceTimersByTimeAsync(0);
		expect(retry.settled).toBe(true);
		expect(retry.failure).toBeUndefined();
		expect(retry.value).toEqual(operation.value);
		expect(fetchFn).toHaveBeenCalledTimes(2);
		expect(signals[0]?.aborted).toBe(true);
		expect(signals[1]).toBeInstanceOf(AbortSignal);
		expect(signals[1]).not.toBe(signals[0]);
		expect(signals[1]?.aborted).toBe(false);
		assertClean();
	});

	it('preserves fast success and clears timers/listeners before a later request', async () => {
		const assertClean = auditCompletion();
		const signals: (AbortSignal | undefined)[] = [];
		const fetchFn = vi.fn(async (_input: unknown, init?: RequestInit) => {
			signals.push(init?.signal ?? undefined);
			expect(init?.method ?? 'GET').toBe(operation.method);
			return jsonResponse(operation.body);
		});
		const state = track(operation.call(fetchFn as typeof fetch));
		await vi.advanceTimersByTimeAsync(0);
		expect(state.settled).toBe(true);
		expect(state.failure).toBeUndefined();
		expect(state.value).toEqual(operation.value);
		assertClean();

		await vi.advanceTimersByTimeAsync(OBSERVATION_CAP_MS);
		expect(signals[0]?.aborted ?? false).toBe(false);
		expect(state.value).toEqual(operation.value);
		expect(models).toEqual([]);
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});

	it.each([
		{ status: 401, code: 'unauthorized', retryable: false, paywall: null },
		{ status: 402, code: 'plan_limit_exceeded', retryable: false, paywall: 'terminal' },
		{ status: 429, code: 'rate_limited', retryable: true, paywall: 'retryable' },
		{ status: 500, code: 'request_failed', retryable: true, paywall: null }
	])('preserves fast HTTP $status classification and clears timers/listeners', async (fixture) => {
		const assertClean = auditCompletion();
		const fetchFn = vi.fn(async () => jsonResponse({
			code: fixture.code, message: 'fixture failure', retryable: fixture.retryable
		}, fixture.status));
		const state = track(operation.call(fetchFn as typeof fetch));
		await vi.advanceTimersByTimeAsync(0);
		expect(state.settled).toBe(true);
		expect(state.failure).toBeInstanceOf(BillingApiError);
		const failure = state.failure as BillingApiError;
		expect(failure.status).toBe(fixture.status);
		expect(failure.code).toBe(fixture.code);
		expect(failure.retryable).toBe(fixture.retryable);
		expect(failure.paywall?.kind ?? null).toBe(fixture.paywall);
		expect(models).toHaveLength(fixture.paywall === null ? 0 : 1);
		expect(signupUrlForCheckoutFailure('plus', failure)).toBe(
			fixture.status === 401 ? '/signup?plan=plus' : null
		);
		assertClean();
		await vi.advanceTimersByTimeAsync(OBSERVATION_CAP_MS);
		expect(state.failure).toBe(failure);
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});

	it('preserves typed non-JSON success failures and clears their lifetime', async () => {
		const assertClean = auditCompletion();
		const fetchFn = vi.fn(async () => new Response('<html>decode sentinel</html>', { status: 200 }));
		const state = track(operation.call(fetchFn as typeof fetch));
		await vi.advanceTimersByTimeAsync(0);
		expect(state.settled).toBe(true);
		expect(state.failure).toBeInstanceOf(BillingApiError);
		expect(state.failure).not.toBeInstanceOf(SyntaxError);
		expect(state.failure).toMatchObject({
			status: 200, code: 'request_failed', retryable: false, paywall: null
		});
		expect((state.failure as Error).message).not.toContain('decode sentinel');
		expect(models).toEqual([]);
		assertClean();
	});

	it('settles an immediate transport failure without leaking a deadline timer/listener', async () => {
		const assertClean = auditCompletion();
		const fetchFn = vi.fn(async () => { throw new TypeError('synthetic network failure'); });
		const state = track(operation.call(fetchFn as typeof fetch));
		await vi.advanceTimersByTimeAsync(0);
		expect(state.settled).toBe(true);
		expect(state.failure).toBeInstanceOf(Error);
		expect(signupUrlForCheckoutFailure('plus', state.failure)).toBeNull();
		expect(models).toEqual([]);
		assertClean();
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});
});
