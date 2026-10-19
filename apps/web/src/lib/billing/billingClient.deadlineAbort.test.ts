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
 * Pin the JSON-body deadline races in `billingClient.requestJson`.
 *
 * The protected deadline suite's `stalledBody` fixture errors its own stream
 * from the abort signal, so settlement there is supplied by
 * `controller.abort()` and the `Promise.race([res.json(), deadline])` reads
 * are never exercised. These cases use abort-ignoring body streams — a
 * partial chunk is enqueued and the stream never listens to the signal, never
 * closes and never errors — so only the deadline race can settle the caller.
 * Removing either body race hangs these cases past the observation cap.
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
}

const OPERATIONS: Operation[] = [
	{ name: 'plans', call: fetchPlans, body: { plans: [PLAN] } },
	{ name: 'usage', call: fetchUsage, body: USAGE },
	{ name: 'subscription', call: fetchSubscription, body: SUBSCRIPTION },
	{ name: 'checkout', call: (fetchFn) => startCheckout('plus', fetchFn), body: { url: CHECKOUT_URL } },
	{ name: 'portal', call: openPortal, body: { url: PORTAL_URL } }
];

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

/** Body stream that ignores the abort signal: only the deadline race settles the caller. */
function abortIgnoringBody(status: number) {
	let controller!: ReadableStreamDefaultController<Uint8Array>;
	let released = false;
	const stream = new ReadableStream<Uint8Array>({
		start(value) {
			controller = value;
			controller.enqueue(new TextEncoder().encode('{"pending":'));
		},
		cancel() {
			released = true;
		}
	});
	const release = () => {
		if (released) return;
		released = true;
		try {
			controller.error(new Error('fixture cleanup'));
		} catch {
			/* stream already disturbed by the reader; cleanup only */
		}
	};
	cleanup.push(release);
	return {
		response: new Response(stream, { status, headers: { 'content-type': 'application/json' } }),
		release
	};
}

const cleanup: (() => void)[] = [];

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

describe.each(OPERATIONS)('billing body deadline without abort cooperation: $name', (operation) => {
	it('settles an abort-ignoring stalled success body with the typed timeout', async () => {
		let signal: AbortSignal | undefined;
		const body = abortIgnoringBody(200);
		const fetchFn = vi.fn(async (_input: unknown, init?: RequestInit) => {
			signal = init?.signal ?? undefined;
			return body.response;
		});
		const state = track(operation.call(fetchFn as typeof fetch));
		await vi.advanceTimersByTimeAsync(0);
		expect(body.response.bodyUsed, 'exercise a pending body read, not hung headers').toBe(true);
		expect(state.settled).toBe(false);

		await vi.advanceTimersByTimeAsync(OBSERVATION_CAP_MS);
		expectDeadline(state);
		expect(signal).toBeInstanceOf(AbortSignal);
		expect(signal?.aborted).toBe(true);
		expect(models, 'a timed-out success read must not publish a paywall').toEqual([]);
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});

	it('settles an abort-ignoring stalled error body with the typed timeout', async () => {
		let signal: AbortSignal | undefined;
		const body = abortIgnoringBody(402);
		const fetchFn = vi.fn(async (_input: unknown, init?: RequestInit) => {
			signal = init?.signal ?? undefined;
			return body.response;
		});
		const state = track(operation.call(fetchFn as typeof fetch));
		await vi.advanceTimersByTimeAsync(0);
		expect(body.response.bodyUsed, 'exercise a pending error-body read, not hung headers').toBe(true);
		expect(state.settled).toBe(false);

		await vi.advanceTimersByTimeAsync(OBSERVATION_CAP_MS);
		expectDeadline(state);
		expect(signal).toBeInstanceOf(AbortSignal);
		expect(signal?.aborted).toBe(true);
		expect(models, 'a timed-out error read must not publish a paywall').toEqual([]);
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});
});
