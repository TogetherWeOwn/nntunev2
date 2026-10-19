import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
	clearSnapshot,
	LIBRARY_AUTH_TRANSITION_KEY,
	loadSnapshot,
	saveSnapshot,
	type SnapshotStorage
} from './cache.js';
import { createLibraryRefresher, type LibraryRefresher } from './refresh.js';

// Ordinary HTTP refusals, not losing timeout/supersession races. Omitting
// cancellation leaks the producer; draining its content or awaiting cleanup
// hangs the refresh. Fake time never reaches the request deadline here.
const STATUSES = [404, 401, 403, 503] as const;
const CANCEL_MODES = ['resolve', 'reject', 'throw', 'pending'] as const;
type CancelMode = (typeof CANCEL_MODES)[number];

const refreshers: LibraryRefresher[] = [];
const bodies: ReadableStream<Uint8Array>[] = [];

beforeEach(() => vi.useFakeTimers());
afterEach(async () => {
	for (const refresher of refreshers.splice(0)) refresher.dispose();
	for (const body of bodies.splice(0)) {
		if (!body.locked) void body.cancel().catch(() => {});
	}
	vi.restoreAllMocks();
	vi.useRealTimers();
	// Let Vitest observe any unhandled cleanup rejection, without installing
	// a listener that could accidentally hide one from the runner.
	await new Promise((resolve) => setTimeout(resolve, 0));
});

function memoryStorage(): SnapshotStorage {
	const values = new Map<string, string>();
	return {
		getItem: (key) => values.get(key) ?? null,
		setItem: (key, value) => {
			values.set(key, value);
		},
		removeItem: (key) => {
			values.delete(key);
		}
	};
}

function fixture() {
	const storage = memoryStorage();
	let invalidations = 0;
	const refresher = createLibraryRefresher({
		sessionStorage: () => storage,
		invalidateLibraryCache: () => {
			invalidations += 1;
			clearSnapshot(storage);
		},
		timeoutMs: 60_000
	});
	refreshers.push(refresher);
	return { refresher, storage, invalidations: () => invalidations };
}

function refusal(status: number, mode: CancelMode = 'resolve') {
	let pulls = 0;
	const cancelSource = vi.fn(() => {
		if (mode === 'throw') throw new Error('synthetic cancel throw');
		if (mode === 'reject') return Promise.reject(new Error('synthetic cancel rejection'));
		if (mode === 'pending') return new Promise<void>(() => {});
	});
	const stream = new ReadableStream<Uint8Array>({
		pull(controller) {
			pulls += 1;
			// A plausible payload must still be ignored on a refusal. Never
			// closing means a drain/JSON parse cannot masquerade as cleanup.
			controller.enqueue(new TextEncoder().encode('[{"id":"forbidden","title":"Private"}]'));
		},
		cancel: cancelSource
	}, { highWaterMark: 0 }); // no automatic prefetch: any pull means a read
	bodies.push(stream);
	const response = new Response(stream, {
		status,
		headers: { 'content-type': 'application/json' }
	});
	const cancelBody = vi.spyOn(stream, 'cancel');
	const observed: { signal: AbortSignal | null } = { signal: null };
	const fetchFn = (async (_input: RequestInfo | URL, init?: RequestInit) => {
		observed.signal = init?.signal ?? null;
		return response;
	}) as typeof fetch;
	return { fetchFn, response, cancelSource, cancelBody, observed, pulls: () => pulls };
}

function okFetch(id = 'fresh'): typeof fetch {
	return (async () => new Response(JSON.stringify([{ id, title: `Track ${id}` }]), {
		headers: { 'content-type': 'application/json' }
	})) as typeof fetch;
}

async function flushMicrotasks(): Promise<void> {
	for (let i = 0; i < 40; i++) await Promise.resolve();
}

async function expectSettled(refresh: Promise<void>): Promise<void> {
	let outcome = 'pending';
	void refresh.then(() => { outcome = 'resolved'; }, () => { outcome = 'rejected'; });
	await flushMicrotasks();
	expect(outcome, 'refresh must settle without body EOF, cleanup completion, or a timeout').toBe(
		'resolved'
	);
}

function expectRefusalState(refresher: LibraryRefresher, status: number): void {
	expect(refresher.state.loading).toBe(false);
	expect(refresher.state.items).toEqual([]);
	expect(refresher.state.staleAt).toBeNull();
	expect(refresher.state.libraryUnavailable).toBe(status === 404);
	expect(refresher.state.loadError).toBe(status === 404 ? null : status === 503
		? 'Library is not reachable right now.'
		: 'You are signed out or do not have access to this library.');
}

async function expectRecovery(refresher: LibraryRefresher, storage: SnapshotStorage): Promise<void> {
	await expectSettled(refresher.refresh(okFetch()));
	expect(refresher.state.items).toEqual([{ id: 'fresh', title: 'Track fresh' }]);
	expect(refresher.state.loading).toBe(false);
	expect(refresher.state.loadError).toBeNull();
	expect(refresher.state.libraryUnavailable).toBe(false);
	expect(refresher.state.staleAt).toBeNull();
	expect(loadSnapshot(storage)?.items).toEqual([{ id: 'fresh', title: 'Track fresh' }]);
}

describe.each(CANCEL_MODES)('ordinary HTTP refusal with %s cancellation', (mode) => {
	it.each(STATUSES)('releases an unread %s body exactly once and permits another refresh', async (status) => {
		const { refresher, storage, invalidations } = fixture();
		const body = refusal(status, mode);

		await expectSettled(refresher.refresh(body.fetchFn));

		expectRefusalState(refresher, status);
		expect(invalidations()).toBe(status === 401 || status === 403 ? 1 : 0);
		expect(body.observed.signal).not.toBeNull();
		expect(body.observed.signal?.aborted).toBe(false);
		expect(body.pulls(), 'refusal content must not be read').toBe(0);
		expect(vi.getTimerCount(), 'the request deadline must be cleared').toBe(0);
		// This must happen on the refusal exit, before another refresh or
		// dispose could rescue an abandoned body. Soft so RED also exercises
		// the subsequent-refresh and double-release controls below.
		expect.soft(body.cancelSource, 'cancel before recovery or disposal').toHaveBeenCalledTimes(1);
		await expectRecovery(refresher, storage);
		// A subsequent generation and disposal must not re-release an old
		// response or let its cleanup overwrite the successful state.
		refresher.dispose();
		await flushMicrotasks();
		expect(refresher.state.items.map((item) => item.id)).toEqual(['fresh']);
		expect(body.cancelSource, 'the refusal producer must be cancelled').toHaveBeenCalledTimes(1);
		// A reader-based cancel is valid too; do not require body.cancel as
		// the mechanism, but catch duplicate direct cancellation attempts.
		expect(body.cancelBody.mock.calls.length).toBeLessThanOrEqual(1);
	});
});

describe('HTTP refusal preserves cache and state semantics', () => {
	it.each([404, 401, 403])('%s never exposes the authenticated snapshot as stale', async (status) => {
		const { refresher, storage, invalidations } = fixture();
		await expectSettled(refresher.refresh(okFetch('prior')));
		expect(loadSnapshot(storage)?.items.map((item) => item.id)).toEqual(['prior']);
		const body = refusal(status);

		await expectSettled(refresher.refresh(body.fetchFn));

		expectRefusalState(refresher, status);
		if (status === 401 || status === 403) {
			expect(invalidations()).toBe(1);
			expect(loadSnapshot(storage)).toBeNull();
			// A generic failure after sign-out must not regain stale access.
			await expectSettled(refresher.refresh((async () => { throw new Error('offline'); }) as typeof fetch));
			expect(refresher.state.items).toEqual([]);
			expect(refresher.state.staleAt).toBeNull();
			expect(refresher.state.loadError).toBe('Library is not reachable right now.');
		} else {
			expect(invalidations()).toBe(0);
		}
		await expectRecovery(refresher, storage);
		expect(body.pulls()).toBe(0);
		expect(body.cancelSource).toHaveBeenCalledTimes(1);
	});

	it('503 keeps an authenticated stale snapshot even when cancellation never settles', async () => {
		const { refresher, storage, invalidations } = fixture();
		await expectSettled(refresher.refresh(okFetch('prior')));
		const snapshot = loadSnapshot(storage);
		expect(snapshot).not.toBeNull();
		const body = refusal(503, 'pending');

		await expectSettled(refresher.refresh(body.fetchFn));

		expect(refresher.state.items).toEqual([{ id: 'prior', title: 'Track prior' }]);
		expect(refresher.state.staleAt).toBe(snapshot?.savedAt);
		expect(refresher.state.loadError).toBeNull();
		expect(refresher.state.loading).toBe(false);
		expect(refresher.state.libraryUnavailable).toBe(false);
		expect(invalidations()).toBe(0);
		await expectRecovery(refresher, storage);
		expect(body.observed.signal?.aborted).toBe(false);
		expect(body.pulls()).toBe(0);
		expect(body.cancelSource).toHaveBeenCalledTimes(1);
	});

	it('503 does not authenticate a snapshot loaded from storage alone', async () => {
		const { refresher, storage } = fixture();
		saveSnapshot(storage, [{ id: 'other-session', title: 'Other session' }], 1_000);
		const body = refusal(503);

		await expectSettled(refresher.refresh(body.fetchFn));

		expectRefusalState(refresher, 503);
		await expectRecovery(refresher, storage);
		expect(body.cancelSource).toHaveBeenCalledTimes(1);
	});

	it.each(STATUSES)('handles a body-less %s without disrupting state or recovery', async (status) => {
		const { refresher, storage } = fixture();
		await expectSettled(refresher.refresh((async () => new Response(null, { status })) as typeof fetch));
		expectRefusalState(refresher, status);
		await expectRecovery(refresher, storage);
	});

	it('consumes a successful streamed JSON payload without cancelling its producer', async () => {
		const { refresher, storage } = fixture();
		const bytes = new TextEncoder().encode('[{"id":"utf8","title":"Café ♪"}]');
		const split = bytes.indexOf(0xe2) + 1;
		const chunks = [bytes.slice(0, split), bytes.slice(split)];
		const cancel = vi.fn();
		const stream = new ReadableStream<Uint8Array>({
			pull(controller) {
				const chunk = chunks.shift();
				if (chunk) controller.enqueue(chunk);
				else controller.close();
			},
			cancel
		}, { highWaterMark: 0 });
		bodies.push(stream);

		await expectSettled(refresher.refresh((async () => new Response(stream)) as typeof fetch));

		expect(refresher.state.items).toEqual([{ id: 'utf8', title: 'Café ♪' }]);
		expect(loadSnapshot(storage)?.items).toEqual([{ id: 'utf8', title: 'Café ♪' }]);
		expect(refresher.state.loadError).toBeNull();
		expect(refresher.state.loading).toBe(false);
		expect(cancel).not.toHaveBeenCalled();
	});

	it('a refusal after an auth transition cannot restore the invalidated snapshot', async () => {
		const { refresher, storage } = fixture();
		await expectSettled(refresher.refresh(okFetch('prior')));
		refresher.handleAuthTransition({ key: LIBRARY_AUTH_TRANSITION_KEY });
		const body = refusal(503);

		await expectSettled(refresher.refresh(body.fetchFn));

		expectRefusalState(refresher, 503);
		expect(loadSnapshot(storage)).toBeNull();
		await expectRecovery(refresher, storage);
		expect(body.cancelSource).toHaveBeenCalledTimes(1);
	});
});
