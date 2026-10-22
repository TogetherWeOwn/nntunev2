import { describe, expect, it, vi } from 'vitest';

import {
	LIBRARY_AUTH_TRANSITION_KEY,
	loadSnapshot,
	saveSnapshot,
	type SnapshotStorage
} from '#lib/library/cache.js';
import { createLibraryRefresher, type LibraryRefresher } from '#lib/library/refresh.js';

/**
 * Acceptance: whichever library-refresh race branch loses — the timeout
 * fires first, or a newer generation (auth transition / dispose) supersedes
 * the request — the losing response body must be cancelled, not abandoned
 * with its stream held open. All fetches are injected with controllable
 * streams; no network. A real `ReadableStream` source counts `cancel()`
 * calls, so "released" is observed at the stream, not inferred from mocks.
 * (The injected fetches deliberately ignore the abort signal, like a
 * response that arrives after its abort already fired: abort alone cannot
 * release a body that materializes late.)
 */

const FAST_TIMEOUT_MS = 25;
const SLOW_TIMEOUT_MS = 5_000;

interface TrackedBody {
	stream: ReadableStream<Uint8Array>;
	cancels: number;
	pulls: number;
	enqueueBytes: (bytes: Uint8Array) => void;
	enqueueText: (text: string) => void;
	close: () => void;
}

function trackedBody(options: { cancelError?: Error } = {}): TrackedBody {
	let controller!: ReadableStreamDefaultController<Uint8Array>;
	const tracked: TrackedBody = {
		stream: null as unknown as ReadableStream<Uint8Array>,
		cancels: 0,
		pulls: 0,
		enqueueBytes: (bytes) => controller.enqueue(bytes),
		enqueueText: (text) => controller.enqueue(new TextEncoder().encode(text)),
		close: () => controller.close()
	};
	tracked.stream = new ReadableStream<Uint8Array>({
		start(c) {
			controller = c;
		},
		pull() {
			tracked.pulls += 1;
		},
		cancel() {
			tracked.cancels += 1;
			if (options.cancelError) throw options.cancelError;
		}
	});
	return tracked;
}

function memoryStorage(): SnapshotStorage {
	const map = new Map<string, string>();
	return {
		getItem: (key) => map.get(key) ?? null,
		setItem: (key, value) => {
			map.set(key, value);
		},
		removeItem: (key) => {
			map.delete(key);
		}
	};
}

function makeRefresher(timeoutMs: number = FAST_TIMEOUT_MS): {
	refresher: LibraryRefresher;
	storage: SnapshotStorage;
	invalidations: () => number;
} {
	const storage = memoryStorage();
	let invalidateCount = 0;
	const refresher = createLibraryRefresher({
		sessionStorage: () => storage,
		invalidateLibraryCache: () => {
			invalidateCount += 1;
		},
		timeoutMs
	});
	return { refresher, storage, invalidations: () => invalidateCount };
}

interface Deferred<T> {
	promise: Promise<T>;
	resolve: (value: T) => void;
	reject: (error: unknown) => void;
}

function deferred<T>(): Deferred<T> {
	let resolve!: (value: T) => void;
	let reject!: (error: unknown) => void;
	const promise = new Promise<T>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	return { promise, resolve, reject };
}

async function flush(rounds = 6): Promise<void> {
	for (let i = 0; i < rounds; i++) await new Promise((r) => setTimeout(r, 0));
}

async function expectCancelled(body: TrackedBody): Promise<void> {
	await vi.waitFor(() => expect(body.cancels).toBeGreaterThan(0), {
		timeout: 750,
		interval: 10
	});
}

function jsonResponse(body: ReadableStream<Uint8Array> | string, status = 200): Response {
	return new Response(body, {
		status,
		headers: { 'content-type': 'application/json' }
	});
}

function okFetch(payload: unknown = [{ id: 'fresh', title: 'Fresh track' }]): typeof fetch {
	return (async () => jsonResponse(JSON.stringify(payload))) as typeof fetch;
}

async function expectSubsequentRefreshSucceeds(refresher: LibraryRefresher): Promise<void> {
	await refresher.refresh(okFetch());
	expect(refresher.state.items.map((i) => i.id)).toEqual(['fresh']);
	expect(refresher.state.loadError).toBeNull();
	expect(refresher.state.loading).toBe(false);
}

describe('losing race branches cancel the response body', () => {
	it('cancels a response that arrives after the timeout already won the fetch race', async () => {
		const { refresher } = makeRefresher();
		const late = deferred<Response>();
		const observed: { signal: AbortSignal | null } = { signal: null };
		const fetchFn = ((_input: RequestInfo | URL, init?: RequestInit) => {
			observed.signal = init?.signal ?? null;
			return late.promise;
		}) as typeof fetch;

		await refresher.refresh(fetchFn);

		// The timeout won: it aborted the request and the refresh settled.
		expect(observed.signal?.aborted).toBe(true);
		expect(refresher.state.loadError).toBe('Library is not reachable right now.');
		expect(refresher.state.loading).toBe(false);

		const body = trackedBody();
		late.resolve(jsonResponse(body.stream));
		await expectCancelled(body);

		await expectSubsequentRefreshSucceeds(refresher);
	});

	it('cancels the body when the timeout wins while the payload is still streaming in', async () => {
		const { refresher } = makeRefresher();
		const body = trackedBody(); // never enqueues: the parse hangs mid-body
		const fetchFn = (async () => jsonResponse(body.stream)) as typeof fetch;

		await refresher.refresh(fetchFn);

		expect(refresher.state.loadError).toBe('Library is not reachable right now.');
		await expectCancelled(body);

		await expectSubsequentRefreshSucceeds(refresher);
	});

	it('cancels a response superseded by an auth transition while the fetch is pending', async () => {
		const { refresher } = makeRefresher(SLOW_TIMEOUT_MS);
		const late = deferred<Response>();
		const fetchFn = (() => late.promise) as unknown as typeof fetch;

		const refreshPromise = refresher.refresh(fetchFn);
		refresher.handleAuthTransition({ key: LIBRARY_AUTH_TRANSITION_KEY });
		expect(refresher.state.loadError).toBe(
			'Your session changed. Sign in again to see this library.'
		);

		const body = trackedBody();
		late.resolve(jsonResponse(body.stream));
		await refreshPromise;
		await expectCancelled(body);

		// The superseded response must not overwrite the session-changed state.
		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.loadError).toBe(
			'Your session changed. Sign in again to see this library.'
		);

		await expectSubsequentRefreshSucceeds(refresher);
	});

	it('cancels the body when an auth transition supersedes a mid-stream parse', async () => {
		const { refresher } = makeRefresher(SLOW_TIMEOUT_MS);
		const body = trackedBody(); // hangs: nothing is ever enqueued
		const fetchFn = (async () => jsonResponse(body.stream)) as typeof fetch;

		const refreshPromise = refresher.refresh(fetchFn);
		await flush(); // let the parse start reading the body
		refresher.handleAuthTransition({ key: LIBRARY_AUTH_TRANSITION_KEY });

		// Releasing the body is what lets the superseded refresh settle at all
		// without waiting for its (long) timeout.
		await expectCancelled(body);
		await refreshPromise;

		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.loadError).toBe(
			'Your session changed. Sign in again to see this library.'
		);

		await expectSubsequentRefreshSucceeds(refresher);
	});

	it('cancels a response that arrives after dispose superseded its fetch', async () => {
		const { refresher } = makeRefresher(SLOW_TIMEOUT_MS);
		const late = deferred<Response>();
		const fetchFn = (() => late.promise) as unknown as typeof fetch;

		const refreshPromise = refresher.refresh(fetchFn);
		refresher.dispose();

		const body = trackedBody();
		late.resolve(jsonResponse(body.stream));
		await refreshPromise;
		await expectCancelled(body);
	});

	it('swallows a failing body cancel and still refreshes afterwards', async () => {
		const { refresher } = makeRefresher();
		const late = deferred<Response>();
		const fetchFn = (() => late.promise) as unknown as typeof fetch;

		await refresher.refresh(fetchFn); // timeout wins

		const body = trackedBody({ cancelError: new Error('cancel blew up') });
		late.resolve(jsonResponse(body.stream));
		// The cancel must be attempted; its failure must not surface as an
		// unhandled rejection (vitest fails the file on those) or corrupt state.
		await expectCancelled(body);
		await flush();
		expect(refresher.state.loadError).toBe('Library is not reachable right now.');

		await expectSubsequentRefreshSucceeds(refresher);
	});
});

describe('preserved refresh behavior', () => {
	it('parses a streamed multi-chunk payload, saves the snapshot, and clears the timeout', async () => {
		const { refresher, storage } = makeRefresher();
		const body = trackedBody();
		const bytes = new TextEncoder().encode('[{"id":"a","title":"Café ♪"}]');
		const noteStart = bytes.indexOf(0xe2); // first byte of '♪' (3-byte sequence)
		body.enqueueBytes(bytes.slice(0, noteStart + 1)); // split mid-character
		body.enqueueBytes(bytes.slice(noteStart + 1));
		body.close();

		const requests: Array<{ url: string; credentials?: string; signal: AbortSignal | null }> = [];
		const fetchFn = (async (input: RequestInfo | URL, init?: RequestInit) => {
			requests.push({
				url: String(input),
				credentials: init?.credentials,
				signal: init?.signal ?? null
			});
			return jsonResponse(body.stream);
		}) as typeof fetch;

		await refresher.refresh(fetchFn);

		expect(requests).toHaveLength(1);
		expect(requests[0]?.url).toBe('/api/library');
		expect(requests[0]?.credentials).toBe('include');
		expect(refresher.state.items).toEqual([{ id: 'a', title: 'Café ♪' }]);
		expect(refresher.state.staleAt).toBeNull();
		expect(loadSnapshot(storage)?.items.map((i) => i.id)).toEqual(['a']);

		// The timeout was cleared on success: it must not fire late and abort
		// the completed request or flip the settled state.
		await new Promise((r) => setTimeout(r, FAST_TIMEOUT_MS + 40));
		expect(requests[0]?.signal?.aborted).toBe(false);
		expect(refresher.state.items.map((i) => i.id)).toEqual(['a']);
		expect(refresher.state.loadError).toBeNull();
	});

	it('404 marks the library unavailable and does not expose a prior snapshot', async () => {
		const { refresher } = makeRefresher();
		await refresher.refresh(okFetch([{ id: 'a', title: 'Track a' }]));
		expect(refresher.state.items.map((i) => i.id)).toEqual(['a']);

		await refresher.refresh((async () =>
			jsonResponse('{"error":"not found"}', 404)) as typeof fetch);

		expect(refresher.state.libraryUnavailable).toBe(true);
		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.staleAt).toBeNull();
		expect(refresher.state.loadError).toBeNull();
		expect(refresher.state.loading).toBe(false);
	});

	it.each([
		[401, 'You are signed out or do not have access to this library.'],
		[403, 'You do not have access to this library.']
	] as const)(
		'%s reports its auth failure, invalidates the cache, and clears shown items',
		async (status, expectedLoadError) => {
			const { refresher, invalidations } = makeRefresher();
			await refresher.refresh(okFetch([{ id: 'a', title: 'Track a' }]));

			await refresher.refresh((async () =>
				jsonResponse('{"error":"denied"}', status)) as typeof fetch);

			expect(refresher.state.loadError).toBe(expectedLoadError);
			expect(refresher.state.items).toEqual([]);
			expect(refresher.state.staleAt).toBeNull();
			expect(invalidations()).toBe(1);
			expect(refresher.state.loading).toBe(false);
		}
	);

	it('falls back to the snapshot after an authenticated success when a later refresh fails', async () => {
		const { refresher } = makeRefresher();
		await refresher.refresh(okFetch([{ id: 'a', title: 'Track a' }]));

		await refresher.refresh((async () => {
			throw new Error('network down');
		}) as typeof fetch);

		expect(refresher.state.items.map((i) => i.id)).toEqual(['a']);
		expect(refresher.state.staleAt).not.toBeNull();
		expect(refresher.state.loadError).toBeNull();
		expect(refresher.state.loading).toBe(false);
	});

	it('does not trust a stored snapshot before this load verified the session', async () => {
		const { refresher, storage } = makeRefresher();
		saveSnapshot(storage, [{ id: 'stale', title: 'Stale track' }], 1_000);

		await refresher.refresh((async () => {
			throw new Error('network down');
		}) as typeof fetch);

		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.staleAt).toBeNull();
		expect(refresher.state.loadError).toBe('Library is not reachable right now.');
	});

	it('treats an invalid-shape 200 payload as unreachable', async () => {
		const { refresher } = makeRefresher();

		await refresher.refresh(okFetch({ error: 'foreign shape' }));

		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.loadError).toBe('Library is not reachable right now.');
	});

	it('treats a 500 as unreachable', async () => {
		const { refresher } = makeRefresher();

		await refresher.refresh((async () =>
			jsonResponse('{"error":"boom"}', 500)) as typeof fetch);

		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.loadError).toBe('Library is not reachable right now.');
		expect(refresher.state.loading).toBe(false);
	});

	it('ignores a concurrent refresh while one is already in flight', async () => {
		const { refresher } = makeRefresher(SLOW_TIMEOUT_MS);
		const late = deferred<Response>();
		let calls = 0;
		const fetchFn = (() => {
			calls += 1;
			return late.promise;
		}) as unknown as typeof fetch;

		const first = refresher.refresh(fetchFn);
		await refresher.refresh(fetchFn); // no-op while active
		expect(calls).toBe(1);

		late.resolve(jsonResponse('[{"id":"a","title":"Track a"}]'));
		await first;
		expect(refresher.state.items.map((i) => i.id)).toEqual(['a']);
	});

	it('ignores storage events for unrelated keys', async () => {
		const { refresher } = makeRefresher(SLOW_TIMEOUT_MS);
		const late = deferred<Response>();
		const fetchFn = (() => late.promise) as unknown as typeof fetch;

		const refreshPromise = refresher.refresh(fetchFn);
		refresher.handleAuthTransition({ key: 'some-other-key' });
		late.resolve(jsonResponse('[{"id":"a","title":"Track a"}]'));
		await refreshPromise;

		expect(refresher.state.items.map((i) => i.id)).toEqual(['a']);
		expect(refresher.state.loadError).toBeNull();
	});

	it('a late fetch rejection after the timeout changes nothing and does not surface', async () => {
		const { refresher } = makeRefresher();
		const late = deferred<Response>();
		const fetchFn = (() => late.promise) as unknown as typeof fetch;

		await refresher.refresh(fetchFn); // timeout wins
		expect(refresher.state.loadError).toBe('Library is not reachable right now.');

		late.reject(new Error('aborted late'));
		await flush();
		expect(refresher.state.loadError).toBe('Library is not reachable right now.');
		expect(refresher.state.items).toEqual([]);

		await expectSubsequentRefreshSucceeds(refresher);
	});
});
