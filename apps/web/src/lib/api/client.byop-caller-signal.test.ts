import { describe, expect, it } from 'vitest';
import {
	ApiClient,
	ApiError,
	type FetchFn,
	type MagicLinkResult,
	type ProviderTestInput,
	type ProviderTestResult
} from './client.js';

/**
 * Planned caller-signal plumbing for the two BYOP test/save helpers: the
 * page aborts its own request/ceremony controllers on auth-transition
 * reset and on unmount, and the helpers must forward that signal into the
 * existing request machinery — including the checks that suppress a late
 * response (and its global paywall publication) when the transport itself
 * ignores abort. Parsing, paywall policy and return contracts are
 * unchanged; only the optional signal is new.
 *
 * The cast below documents the planned signatures so this file type-checks
 * before the helpers grow the parameter; every test here is runtime RED
 * until the signal is actually honored.
 */
interface PlannedByopCallerSignals {
	testProviderConnection(
		input: ProviderTestInput,
		retry?: () => void,
		signal?: AbortSignal
	): Promise<ProviderTestResult>;
	saveProviderConfig(input: ProviderTestInput, signal?: AbortSignal): Promise<MagicLinkResult>;
}

const INPUT: ProviderTestInput = {
	host: 'news.example.test',
	port: 563,
	tls: true,
	allow_insecure_plaintext: false,
	username: 'reader',
	password: 'fixture-password',
	max_connections: 4
};

function abortReason(): DOMException {
	return new DOMException('fixture reset', 'AbortError');
}

/** A transport that ignores abort: only `settle` completes the request. */
function ignoringTransport() {
	const calls: Array<{ url: string | URL | Request; signal: AbortSignal | null | undefined }> = [];
	let settle: ((response: Response) => void) | null = null;
	const fetchFn = (async (url, init) => {
		calls.push({ url: url as string, signal: init?.signal ?? null });
		return new Promise<Response>((resolve) => {
			settle = resolve;
		});
	}) as FetchFn;
	// No-op when nothing was fetched, so a test that settles defensively
	// never hangs and never throws on the correctly-aborted path.
	return { calls, settle: (response: Response) => settle?.(response), fetchFn };
}

/** Flush pending microtasks so a synchronous fetch initiation lands. */
function tick(): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, 0));
}

function jsonResponse(status: number, body: unknown, headers?: Record<string, string>): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json', ...headers }
	});
}

describe('testProviderConnection caller signal', () => {
	it('sends no request when the caller signal is already aborted', async () => {
		const transport = ignoringTransport();
		const client = new ApiClient({ baseUrl: 'https://api.example.test', fetchFn: transport.fetchFn });
		const controller = new AbortController();
		controller.abort(abortReason());
		const pending = (client as unknown as PlannedByopCallerSignals)
			.testProviderConnection(INPUT, undefined, controller.signal)
			.then(
				() => 'fulfilled',
				(error: unknown) => error
			);
		await tick();
		// Settle defensively: a signal-ignoring implementation still has a
		// held request that must complete for the assertion below to run.
		transport.settle(jsonResponse(200, { ok: true }));
		const received = await pending;
		expect(transport.calls).toHaveLength(0);
		expect(received).toBeInstanceOf(DOMException);
		expect((received as DOMException).name).toBe('AbortError');
	});

	it('suppresses a late success that arrives after the caller aborted', async () => {
		const transport = ignoringTransport();
		const client = new ApiClient({ baseUrl: 'https://api.example.test', fetchFn: transport.fetchFn });
		const controller = new AbortController();
		const result = (client as unknown as PlannedByopCallerSignals)
			.testProviderConnection(INPUT, undefined, controller.signal)
			.then(
				(value) => ({ ok: true as const, value }),
				(error: unknown) => ({ ok: false as const, error })
			);
		controller.abort(abortReason());
		// Settle the abort-ignoring transport even when testing an
		// implementation that fails to forward cancellation, so RED leaves
		// no hanging request.
		transport.settle(jsonResponse(200, { ok: true, latencyMs: 3 }));
		const received = await result;
		expect(received.ok).toBe(false);
		if (!received.ok) {
			expect(received.error).toBeInstanceOf(DOMException);
			expect((received.error as DOMException).name).toBe('AbortError');
		}
	});

	it('suppresses a late paywall that arrives after the caller aborted', async () => {
		const transport = ignoringTransport();
		const client = new ApiClient({ baseUrl: 'https://api.example.test', fetchFn: transport.fetchFn });
		const controller = new AbortController();
		const result = (client as unknown as PlannedByopCallerSignals)
			.testProviderConnection(INPUT, () => {}, controller.signal)
			.then(
				(value) => ({ ok: true as const, value }),
				(error: unknown) => ({ ok: false as const, error })
			);
		controller.abort(abortReason());
		transport.settle(
			jsonResponse(429, { code: 'rate_limited', message: 'limit reached' }, { 'Retry-After': '0' })
		);
		const received = await result;
		// The caller abort wins: no ApiError, so no paywall model is
		// published for the abandoned response.
		expect(received.ok).toBe(false);
		if (!received.ok) {
			expect(received.error).toBeInstanceOf(DOMException);
			expect(received.error).not.toBeInstanceOf(ApiError);
		}
	});
});

describe('saveProviderConfig caller signal', () => {
	it('sends no request when the caller signal is already aborted', async () => {
		const transport = ignoringTransport();
		const client = new ApiClient({ baseUrl: 'https://api.example.test', fetchFn: transport.fetchFn });
		const controller = new AbortController();
		controller.abort(abortReason());
		const pending = (client as unknown as PlannedByopCallerSignals)
			.saveProviderConfig(INPUT, controller.signal)
			.then(
				() => 'fulfilled',
				(error: unknown) => error
			);
		await tick();
		// Settle defensively: a signal-ignoring implementation still has a
		// held request that must complete for the assertion below to run.
		transport.settle(jsonResponse(200, { ok: true }));
		const received = await pending;
		expect(transport.calls).toHaveLength(0);
		expect(received).toBeInstanceOf(DOMException);
		expect((received as DOMException).name).toBe('AbortError');
	});

	it('suppresses a late success that arrives after the caller aborted', async () => {
		const transport = ignoringTransport();
		const client = new ApiClient({ baseUrl: 'https://api.example.test', fetchFn: transport.fetchFn });
		const controller = new AbortController();
		const result = (client as unknown as PlannedByopCallerSignals)
			.saveProviderConfig(INPUT, controller.signal)
			.then(
				(value) => ({ ok: true as const, value }),
				(error: unknown) => ({ ok: false as const, error })
			);
		controller.abort(abortReason());
		transport.settle(jsonResponse(200, { ok: true }));
		const received = await result;
		expect(received.ok).toBe(false);
		if (!received.ok) {
			expect(received.error).toBeInstanceOf(DOMException);
			expect((received.error as DOMException).name).toBe('AbortError');
		}
	});
});
