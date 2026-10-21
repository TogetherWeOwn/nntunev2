import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { onPaywall, type PaywallModel } from '#lib/billing/planLimit.js';
import { load } from './+page';

/**
 * Protected acceptance spec: the billing loader dispatches the subscription
 * and usage reads concurrently, so both are in flight before either settles
 * and two hung reads are bounded by ONE existing 30-second request-deadline
 * window, not two serial windows. Each read keeps its own separate outcome:
 * every status combination (401 / 404 / 402 / 429 / 500 / transport failure /
 * success) must produce exactly the page data the serial loader produced for
 * the same pair of outcomes, in either completion order, and the paywall
 * replay must remain whichever limit response was received latest.
 *
 * All transport is injected and deferred by hand; fake timers own the
 * deadline, so nothing here sleeps in wall-clock time. The deadline value is
 * the existing billing-client budget (30s) — this spec pins that it is
 * neither raised nor removed, only overlapped.
 */
const DEADLINE_MS = 30_000;
const SUB_URL = '/v1/me/subscription';
const USAGE_URL = '/v1/me/usage';

const SUBSCRIPTION = {
	planId: 'plus',
	status: 'active',
	currentPeriodEnd: null,
	trialEndsAt: null
};

const USAGE = {
	period: '2026-10',
	streamSecondsMonth: 90 * 3600,
	concurrentNow: 0,
	libraryTracks: 10,
	ingestToday: 0
};

interface PendingCall {
	url: string;
	signal: AbortSignal | undefined;
	resolve: (res: Response) => void;
	reject: (err: unknown) => void;
}

/** Deferred fetch: every call parks until the test answers it explicitly. */
function deferredFetch() {
	const calls: PendingCall[] = [];
	const fetchFn = ((input: unknown, init?: RequestInit) => {
		const url = typeof input === 'string' ? input : (input as URL).toString();
		return new Promise<Response>((resolve, reject) => {
			calls.push({ url, signal: init?.signal ?? undefined, resolve, reject });
		});
	}) as typeof fetch;

	function pendingFor(url: string): PendingCall {
		const call = calls.find((c) => c.url === url);
		expect(call, `${url} must be dispatched before either read settles`).toBeDefined();
		return call!;
	}

	return {
		fetchFn,
		calls,
		dispatchedUrls: () => calls.map((c) => c.url),
		respond(url: string, status: number, body: unknown, headers?: HeadersInit) {
			pendingFor(url).resolve(
				new Response(JSON.stringify(body), { status, headers })
			);
		},
		respondWith(url: string, response: Response) {
			pendingFor(url).resolve(response);
		},
		failTransport(url: string, err: unknown) {
			pendingFor(url).reject(err);
		},
		signalFor(url: string) {
			return calls.find((c) => c.url === url)?.signal;
		}
	};
}

type LoadData = NonNullable<Awaited<ReturnType<typeof load>>>;

function startLoad(fetchFn: typeof fetch) {
	const promise = load({ fetch: fetchFn } as never) as Promise<LoadData>;
	const state: { settled: boolean; value: LoadData | undefined } = {
		settled: false,
		value: undefined
	};
	void promise.then((value) => {
		state.settled = true;
		state.value = value;
	});
	return { promise, state };
}

/** Flush microtasks/stream reads without consuming the fake deadline. */
async function flush() {
	await vi.advanceTimersByTimeAsync(0);
	await vi.advanceTimersByTimeAsync(0);
}

async function settledData(state: { settled: boolean; value: LoadData | undefined }) {
	await flush();
	expect(state.settled, 'loader must settle once both reads settle').toBe(true);
	return state.value!;
}

/**
 * Start a load over deferred transport, prove both reads were dispatched
 * before either settles, then answer them in the given order.
 */
async function loadInOrder(
	first: (t: ReturnType<typeof deferredFetch>) => void,
	second: (t: ReturnType<typeof deferredFetch>) => void
) {
	const transport = deferredFetch();
	const { state } = startLoad(transport.fetchFn);
	await flush();
	expect(
		[...transport.dispatchedUrls()].sort(),
		'subscription and usage must both dispatch before either read settles'
	).toEqual([SUB_URL, USAGE_URL].sort());
	first(transport);
	await flush();
	expect(state.settled, 'loader must keep waiting for the slower read').toBe(false);
	second(transport);
	return { transport, data: await settledData(state) };
}

function drainPendingPaywall() {
	// Subscribing replays (and clears) any queued limit model, so a failed
	// paywall test cannot leak its pending model into the next one.
	const unsubscribe = onPaywall(() => {});
	unsubscribe();
}

beforeEach(() => {
	vi.useFakeTimers();
});

afterEach(async () => {
	drainPendingPaywall();
	vi.unstubAllGlobals();
	await vi.advanceTimersByTimeAsync(0);
	vi.clearAllTimers();
	vi.useRealTimers();
});

describe('concurrent dispatch', () => {
	it('dispatches subscription and usage before either read settles', async () => {
		const transport = deferredFetch();
		const { state } = startLoad(transport.fetchFn);
		await flush();

		expect([...transport.dispatchedUrls()].sort()).toEqual([SUB_URL, USAGE_URL].sort());
		expect(state.settled).toBe(false);

		transport.respond(SUB_URL, 200, SUBSCRIPTION);
		transport.respond(USAGE_URL, 200, USAGE);
		await settledData(state);
	});

	it('keeps waiting when only the usage read (answered first) has settled', async () => {
		const { data } = await loadInOrder(
			(t) => t.respond(USAGE_URL, 200, USAGE),
			(t) => t.respond(SUB_URL, 200, SUBSCRIPTION)
		);

		expect(data.subscription).toEqual(SUBSCRIPTION);
		expect(data.usage).toEqual(USAGE);
		expect(data.planId).toBe('plus');
		expect(data.planUnknown).toBe(false);
		expect(data.usageStatus).not.toBeNull();
		expect(data.loadError).toBeNull();
		expect(data.hadFetchError).toBe(false);
		expect(vi.getTimerCount(), 'completed reads must not retain deadline timers').toBe(0);
	});

	it('bounds two hung reads by one 30-second deadline window, aborting both', async () => {
		const transport = deferredFetch();
		const { state } = startLoad(transport.fetchFn);
		await flush();
		expect([...transport.dispatchedUrls()].sort()).toEqual([SUB_URL, USAGE_URL].sort());

		await vi.advanceTimersByTimeAsync(DEADLINE_MS - 1);
		expect(state.settled, 'reads must not give up before the existing deadline').toBe(false);

		await vi.advanceTimersByTimeAsync(1);
		await flush();
		expect(
			state.settled,
			'two hung reads must settle inside one deadline window, not a second serial window'
		).toBe(true);
		expect(transport.signalFor(SUB_URL)).toBeInstanceOf(AbortSignal);
		expect(transport.signalFor(USAGE_URL)).toBeInstanceOf(AbortSignal);
		expect(transport.signalFor(SUB_URL)?.aborted).toBe(true);
		expect(transport.signalFor(USAGE_URL)?.aborted).toBe(true);
		expect(vi.getTimerCount()).toBe(0);

		const data = state.value!;
		expect(data.signedOut).toBe(false);
		expect(data.hadFetchError).toBe(true);
		expect(data.loadError).toMatch(/not reachable/i);
		expect(data.subscription).toBeNull();
		expect(data.usage).toBeNull();
	});

	it('bounds a stalled response body by the same window while the peer succeeds', async () => {
		const transport = deferredFetch();
		const { state } = startLoad(transport.fetchFn);
		await flush();
		expect([...transport.dispatchedUrls()].sort()).toEqual([SUB_URL, USAGE_URL].sort());

		// Subscription headers arrive but the JSON body never finishes.
		const subSignal = transport.signalFor(SUB_URL);
		let bodyController!: ReadableStreamDefaultController<Uint8Array>;
		const stalled = new ReadableStream<Uint8Array>({
			start(controller) {
				bodyController = controller;
				controller.enqueue(new TextEncoder().encode('{"planId":'));
			}
		});
		subSignal?.addEventListener(
			'abort',
			() => {
				try {
					bodyController.error(subSignal.reason);
				} catch {
					/* already closed */
				}
			},
			{ once: true }
		);
		transport.respondWith(
			SUB_URL,
			new Response(stalled, { status: 200, headers: { 'content-type': 'application/json' } })
		);
		transport.respond(USAGE_URL, 200, USAGE);
		await flush();
		expect(state.settled).toBe(false);

		await vi.advanceTimersByTimeAsync(DEADLINE_MS);
		const data = await settledData(state);
		expect(subSignal?.aborted).toBe(true);
		expect(data.subscription).toBeNull();
		expect(data.planUnknown).toBe(true);
		// The stalled read must not discard its successful peer.
		expect(data.usage).toEqual(USAGE);
		expect(data.usageStatus).toBeNull();
		expect(data.loadError).toMatch(/subscription/i);
		expect(data.hadFetchError).toBe(true);
		expect(vi.getTimerCount()).toBe(0);
	});
});

describe('failure isolation in reversed completion order', () => {
	it('preserves successful usage when the subscription read later fails with 500', async () => {
		const { data } = await loadInOrder(
			(t) => t.respond(USAGE_URL, 200, USAGE),
			(t) => t.respond(SUB_URL, 500, { error: 'boom' })
		);

		expect(data.subscription).toBeNull();
		expect(data.usage).toEqual(USAGE);
		expect(data.planId).toBe('free');
		expect(data.planUnknown).toBe(true);
		expect(data.usageStatus).toBeNull();
		expect(data.loadError).toMatch(/subscription/i);
		expect(data.hadFetchError).toBe(true);
	});

	it('preserves a confirmed subscription when the usage read later fails with 500', async () => {
		const { data } = await loadInOrder(
			(t) => t.respond(USAGE_URL, 500, { error: 'boom' }),
			(t) => t.respond(SUB_URL, 200, SUBSCRIPTION)
		);

		expect(data.subscription).toEqual(SUBSCRIPTION);
		expect(data.planId).toBe('plus');
		expect(data.planUnknown).toBe(false);
		expect(data.usage).toBeNull();
		expect(data.usageStatus).toBeNull();
		expect(data.loadError).toMatch(/could not load usage/i);
		expect(data.hadFetchError).toBe(true);
	});

	it('keeps a usage 404 nonfatal when it arrives before the subscription', async () => {
		const { data } = await loadInOrder(
			(t) => t.respond(USAGE_URL, 404, { error: 'not found' }),
			(t) => t.respond(SUB_URL, 200, SUBSCRIPTION)
		);

		expect(data.subscription).toEqual(SUBSCRIPTION);
		expect(data.planUnknown).toBe(false);
		expect(data.usage).toBeNull();
		expect(data.usageStatus).toBeNull();
		expect(data.loadError).toBeNull();
		expect(data.hadFetchError).toBe(false);
	});

	it('keeps a subscription 404 a retryable failure when usage succeeded first', async () => {
		const { data } = await loadInOrder(
			(t) => t.respond(USAGE_URL, 200, USAGE),
			(t) => t.respond(SUB_URL, 404, { error: 'not found' })
		);

		expect(data.subscription).toBeNull();
		expect(data.usage).toEqual(USAGE);
		expect(data.planUnknown).toBe(true);
		expect(data.usageStatus).toBeNull();
		expect(data.hadFetchError).toBe(true);
		expect(data.loadError).toMatch(/subscription/i);
	});

	it('reports an unreachable backend when both reads fail, usage first', async () => {
		const { data } = await loadInOrder(
			(t) => t.respond(USAGE_URL, 500, { error: 'boom' }),
			(t) => t.respond(SUB_URL, 500, { error: 'boom' })
		);

		expect(data.signedOut).toBe(false);
		expect(data.subscription).toBeNull();
		expect(data.usage).toBeNull();
		expect(data.loadError).toMatch(/not reachable/i);
		expect(data.hadFetchError).toBe(true);
	});

	it('does not fabricate usage when its transport fails before the subscription settles', async () => {
		const { data } = await loadInOrder(
			(t) => t.failTransport(USAGE_URL, new TypeError('fetch failed')),
			(t) => t.respond(SUB_URL, 200, SUBSCRIPTION)
		);

		expect(data.subscription).toEqual(SUBSCRIPTION);
		expect(data.usage).toBeNull();
		expect(data.usageStatus).toBeNull();
		expect(data.loadError).toMatch(/could not load usage/i);
		expect(data.hadFetchError).toBe(true);
	});

	it('does not fabricate a subscription when its transport fails after usage succeeded', async () => {
		const { data } = await loadInOrder(
			(t) => t.respond(USAGE_URL, 200, USAGE),
			(t) => t.failTransport(SUB_URL, new TypeError('fetch failed'))
		);

		expect(data.subscription).toBeNull();
		expect(data.usage).toEqual(USAGE);
		expect(data.planUnknown).toBe(true);
		expect(data.usageStatus).toBeNull();
		expect(data.loadError).toMatch(/subscription/i);
		expect(data.hadFetchError).toBe(true);
	});
});

describe('signed-out precedence in either completion order', () => {
	it('reports signed-out when a usage 401 arrives before a successful subscription', async () => {
		const { data } = await loadInOrder(
			(t) => t.respond(USAGE_URL, 401, { error: 'unauthorized' }),
			(t) => t.respond(SUB_URL, 200, SUBSCRIPTION)
		);

		expect(data.signedOut).toBe(true);
		expect(data.usageStatus).toBeNull();
		expect(data.loadError).toMatch(/sign in/i);
		expect(data.loadError).not.toMatch(/not reachable/i);
		expect(data.hadFetchError).toBe(false);
	});

	it('reports signed-out when a subscription 401 arrives after a usage 500', async () => {
		const { data } = await loadInOrder(
			(t) => t.respond(USAGE_URL, 500, { error: 'boom' }),
			(t) => t.respond(SUB_URL, 401, { error: 'unauthorized' })
		);

		expect(data.signedOut).toBe(true);
		expect(data.loadError).toMatch(/sign in/i);
		expect(data.loadError).not.toMatch(/not reachable|could not load usage/i);
		expect(data.hadFetchError).toBe(false);
	});

	it('reports signed-out when a subscription 401 arrives before a successful usage', async () => {
		const { data } = await loadInOrder(
			(t) => t.respond(SUB_URL, 401, { error: 'unauthorized' }),
			(t) => t.respond(USAGE_URL, 200, USAGE)
		);

		expect(data.signedOut).toBe(true);
		expect(data.loadError).toMatch(/sign in/i);
		expect(data.hadFetchError).toBe(false);
	});
});

describe('paywall behaviour in either completion order', () => {
	it('replays the latest-received limit: usage 429 first, subscription 402 second', async () => {
		vi.stubGlobal('window', {});
		const { data } = await loadInOrder(
			(t) =>
				t.respond(USAGE_URL, 429, { code: 'rate_limited' }, { 'Retry-After': '45' }),
			(t) => t.respond(SUB_URL, 402, { code: 'plan_limit_exceeded' })
		);

		const seen: PaywallModel[] = [];
		const unsubscribe = onPaywall((model) => seen.push(model));
		unsubscribe();
		expect(seen).toHaveLength(1);
		expect(seen[0]?.status).toBe(402);
		expect(seen[0]?.code).toBe('plan_limit_exceeded');
		expect(seen[0]?.ctaHref).toBe('/pricing');

		expect(data.loadError).toBeNull();
		expect(data.planUnknown).toBe(true);
		// A 429 anywhere keeps the page Retry control available.
		expect(data.hadFetchError).toBe(true);
	});

	it('replays the latest-received limit: subscription 402 first, usage 429 second', async () => {
		vi.stubGlobal('window', {});
		const { data } = await loadInOrder(
			(t) => t.respond(SUB_URL, 402, { code: 'plan_limit_exceeded' }),
			(t) =>
				t.respond(USAGE_URL, 429, { code: 'rate_limited' }, { 'Retry-After': '45' })
		);

		const seen: PaywallModel[] = [];
		const unsubscribe = onPaywall((model) => seen.push(model));
		unsubscribe();
		expect(seen).toHaveLength(1);
		expect(seen[0]?.status).toBe(429);
		expect(seen[0]?.code).toBe('rate_limited');
		expect(seen[0]?.retryAfterSeconds).toBe(45);

		expect(data.loadError).toBeNull();
		expect(data.hadFetchError).toBe(true);
	});

	it('delivers a usage 402 limit and keeps the subscription that arrives after it', async () => {
		vi.stubGlobal('window', {});
		const { data } = await loadInOrder(
			(t) => t.respond(USAGE_URL, 402, { code: 'plan_limit_exceeded' }),
			(t) => t.respond(SUB_URL, 200, SUBSCRIPTION)
		);

		const seen: PaywallModel[] = [];
		const unsubscribe = onPaywall((model) => seen.push(model));
		unsubscribe();
		expect(seen).toHaveLength(1);
		expect(seen[0]?.status).toBe(402);

		expect(data.subscription).toEqual(SUBSCRIPTION);
		expect(data.usage).toBeNull();
		expect(data.loadError).toBeNull();
		expect(data.hadFetchError).toBe(false);
	});

	it('delivers a subscription 429 limit and keeps the usage that arrived before it', async () => {
		vi.stubGlobal('window', {});
		const { data } = await loadInOrder(
			(t) => t.respond(USAGE_URL, 200, USAGE),
			(t) =>
				t.respond(SUB_URL, 429, { code: 'rate_limited' }, { 'Retry-After': '30' })
		);

		const seen: PaywallModel[] = [];
		const unsubscribe = onPaywall((model) => seen.push(model));
		unsubscribe();
		expect(seen).toHaveLength(1);
		expect(seen[0]?.status).toBe(429);
		expect(seen[0]?.retryAfterSeconds).toBe(30);

		expect(data.usage).toEqual(USAGE);
		expect(data.subscription).toBeNull();
		expect(data.planUnknown).toBe(true);
		expect(data.usageStatus).toBeNull();
		expect(data.loadError).toBeNull();
		expect(data.hadFetchError).toBe(true);
	});
});

describe('timing-independent controls (must stay green before and after)', () => {
	function immediateFetch(
		routes: Record<string, { status: number; body: unknown }>,
		signals?: (AbortSignal | undefined)[]
	): typeof fetch {
		return (async (input: unknown, init?: RequestInit) => {
			signals?.push(init?.signal ?? undefined);
			const url = typeof input === 'string' ? input : (input as URL).toString();
			const route = routes[url];
			if (!route) return new Response('not found', { status: 404 });
			return new Response(JSON.stringify(route.body), { status: route.status });
		}) as typeof fetch;
	}

	it('loads a confirmed plan with scored usage from immediate responses', async () => {
		const { state } = startLoad(
			immediateFetch({
				[SUB_URL]: { status: 200, body: SUBSCRIPTION },
				[USAGE_URL]: { status: 200, body: USAGE }
			})
		);
		const data = await settledData(state);

		expect(data.subscription).toEqual(SUBSCRIPTION);
		expect(data.usage).toEqual(USAGE);
		expect(data.planUnknown).toBe(false);
		expect(data.usageStatus).not.toBeNull();
		expect(data.loadError).toBeNull();
		expect(data.hadFetchError).toBe(false);
	});

	it('gives each read its own abort signal and clears both deadline timers', async () => {
		const signals: (AbortSignal | undefined)[] = [];
		const { state } = startLoad(
			immediateFetch(
				{
					[SUB_URL]: { status: 200, body: SUBSCRIPTION },
					[USAGE_URL]: { status: 200, body: USAGE }
				},
				signals
			)
		);
		await settledData(state);

		expect(signals).toHaveLength(2);
		expect(signals[0]).toBeInstanceOf(AbortSignal);
		expect(signals[1]).toBeInstanceOf(AbortSignal);
		expect(signals[0]).not.toBe(signals[1]);
		expect(signals[0]?.aborted).toBe(false);
		expect(signals[1]?.aborted).toBe(false);
		expect(vi.getTimerCount()).toBe(0);
	});

	it('settles one hung usage read at the deadline without losing the subscription', async () => {
		const fetchFn = ((input: unknown) => {
			const url = typeof input === 'string' ? input : (input as URL).toString();
			if (url === USAGE_URL) return new Promise<Response>(() => {});
			return Promise.resolve(
				new Response(JSON.stringify(SUBSCRIPTION), { status: 200 })
			);
		}) as typeof fetch;
		const { state } = startLoad(fetchFn);
		await flush();
		expect(state.settled).toBe(false);

		await vi.advanceTimersByTimeAsync(DEADLINE_MS);
		const data = await settledData(state);
		expect(data.subscription).toEqual(SUBSCRIPTION);
		expect(data.usage).toBeNull();
		expect(data.loadError).toMatch(/could not load usage/i);
		expect(data.hadFetchError).toBe(true);
		expect(vi.getTimerCount()).toBe(0);
	});

	it('settles one hung subscription read at the deadline without losing usage', async () => {
		const fetchFn = ((input: unknown) => {
			const url = typeof input === 'string' ? input : (input as URL).toString();
			if (url === SUB_URL) return new Promise<Response>(() => {});
			return Promise.resolve(new Response(JSON.stringify(USAGE), { status: 200 }));
		}) as typeof fetch;
		const { state } = startLoad(fetchFn);
		await flush();
		expect(state.settled).toBe(false);

		await vi.advanceTimersByTimeAsync(DEADLINE_MS);
		const data = await settledData(state);
		expect(data.subscription).toBeNull();
		expect(data.usage).toEqual(USAGE);
		expect(data.planUnknown).toBe(true);
		expect(data.usageStatus).toBeNull();
		expect(data.loadError).toMatch(/subscription/i);
		expect(data.hadFetchError).toBe(true);
		expect(vi.getTimerCount()).toBe(0);
	});
});
