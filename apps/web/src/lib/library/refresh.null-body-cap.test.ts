import { describe, expect, it } from 'vitest';

import { createLibraryRefresher } from './refresh.js';

/**
 * Null-body fallback cap: runtimes without a readable stream (`body: null`)
 * must enforce `LIBRARY_BODY_MAX_BYTES` via a length-checked `arrayBuffer`
 * read instead of an unbounded `resp.json()`.
 */

const CAP = 1024 * 1024;
const SLOW_TIMEOUT_MS = 5_000;

function payloadOfBytes(targetBytes: number, id = 'a'): string {
	const empty = JSON.stringify([{ id, title: '' }]);
	const pad = targetBytes - empty.length;
	if (pad < 0) throw new Error(`target ${targetBytes} below payload overhead ${empty.length}`);
	const payload = JSON.stringify([{ id, title: 'x'.repeat(pad) }]);
	if (payload.length !== targetBytes) throw new Error('payload construction drifted');
	return payload;
}

function nullBodyResponse(payloadText: string): Response {
	const bytes = new TextEncoder().encode(payloadText);
	const resp = new Response(bytes, {
		status: 200,
		headers: { 'content-type': 'application/json' }
	});
	// Simulate a runtime with no readable stream: the streamed reader path
	// is unavailable, so `readJsonBody` must take the length-checked
	// `arrayBuffer` fallback.
	Object.defineProperty(resp, 'body', { value: null, configurable: true });
	return resp;
}

function nullBodyFetch(payloadText: string): typeof fetch {
	const resp = nullBodyResponse(payloadText);
	return (async () => resp) as unknown as typeof fetch;
}

describe('library refresh null-body cap', () => {
	it('rejects a null-body over-cap payload as unreachable', async () => {
		const refresher = createLibraryRefresher({
			sessionStorage: () => null,
			invalidateLibraryCache: () => {},
			timeoutMs: SLOW_TIMEOUT_MS
		});

		await refresher.refresh(nullBodyFetch(payloadOfBytes(2 * CAP, 'over')));

		expect(refresher.state.items.map((i) => i.id)).toEqual([]);
		expect(refresher.state.loadError).toBe('Library is not reachable right now.');
		expect(refresher.state.loading).toBe(false);
	});

	it('parses a null-body within-cap payload', async () => {
		const refresher = createLibraryRefresher({
			sessionStorage: () => null,
			invalidateLibraryCache: () => {},
			timeoutMs: SLOW_TIMEOUT_MS
		});

		await refresher.refresh(nullBodyFetch(JSON.stringify([{ id: 'small', title: 'ok' }])));

		expect(refresher.state.items.map((i) => i.id)).toEqual(['small']);
		expect(refresher.state.loadError).toBeNull();
		expect(refresher.state.loading).toBe(false);
	});

	/**
	 * Legacy fallback: a response with neither a readable stream nor
	 * `arrayBuffer` (e.g. an injected fetch mock, as in the auth-transition
	 * e2e spec) still parses via `resp.json()` so in-flight bodies settle
	 * through the normal generation race — but stays bounded by the cap.
	 */
	function jsonOnlyFetch(payloadText: string): typeof fetch {
		const data = JSON.parse(payloadText) as unknown;
		const resp = {
			ok: true,
			status: 200,
			body: null,
			json: async () => data
		} as unknown as Response;
		return (async () => resp) as unknown as typeof fetch;
	}

	it('parses a json-only within-cap payload (no arrayBuffer)', async () => {
		const refresher = createLibraryRefresher({
			sessionStorage: () => null,
			invalidateLibraryCache: () => {},
			timeoutMs: SLOW_TIMEOUT_MS
		});

		await refresher.refresh(jsonOnlyFetch(JSON.stringify([{ id: 'legacy', title: 'ok' }])));

		expect(refresher.state.items.map((i) => i.id)).toEqual(['legacy']);
		expect(refresher.state.loadError).toBeNull();
		expect(refresher.state.loading).toBe(false);
	});

	it('rejects a json-only over-cap payload as unreachable', async () => {
		const refresher = createLibraryRefresher({
			sessionStorage: () => null,
			invalidateLibraryCache: () => {},
			timeoutMs: SLOW_TIMEOUT_MS
		});

		await refresher.refresh(jsonOnlyFetch(payloadOfBytes(2 * CAP, 'over')));

		expect(refresher.state.items.map((i) => i.id)).toEqual([]);
		expect(refresher.state.loadError).toBe('Library is not reachable right now.');
		expect(refresher.state.loading).toBe(false);
	});

	it('parses a null-body payload of exactly cap bytes', async () => {
		const refresher = createLibraryRefresher({
			sessionStorage: () => null,
			invalidateLibraryCache: () => {},
			timeoutMs: SLOW_TIMEOUT_MS
		});

		await refresher.refresh(nullBodyFetch(payloadOfBytes(CAP, 'exact')));

		expect(refresher.state.items.map((i) => i.id)).toEqual(['exact']);
		expect(refresher.state.loadError).toBeNull();
		expect(refresher.state.loading).toBe(false);
	});

	it('rejects a null-body payload of cap + 1 bytes', async () => {
		const refresher = createLibraryRefresher({
			sessionStorage: () => null,
			invalidateLibraryCache: () => {},
			timeoutMs: SLOW_TIMEOUT_MS
		});

		await refresher.refresh(nullBodyFetch(payloadOfBytes(CAP + 1, 'one-over')));

		expect(refresher.state.items.map((i) => i.id)).toEqual([]);
		expect(refresher.state.loadError).toBe('Library is not reachable right now.');
		expect(refresher.state.loading).toBe(false);
	});

	it('parses a json-only payload of exactly cap bytes', async () => {
		const refresher = createLibraryRefresher({
			sessionStorage: () => null,
			invalidateLibraryCache: () => {},
			timeoutMs: SLOW_TIMEOUT_MS
		});

		await refresher.refresh(jsonOnlyFetch(payloadOfBytes(CAP, 'exact')));

		expect(refresher.state.items.map((i) => i.id)).toEqual(['exact']);
		expect(refresher.state.loadError).toBeNull();
		expect(refresher.state.loading).toBe(false);
	});

	it('rejects a json-only payload of cap + 1 bytes', async () => {
		const refresher = createLibraryRefresher({
			sessionStorage: () => null,
			invalidateLibraryCache: () => {},
			timeoutMs: SLOW_TIMEOUT_MS
		});

		await refresher.refresh(jsonOnlyFetch(payloadOfBytes(CAP + 1, 'one-over')));

		expect(refresher.state.items.map((i) => i.id)).toEqual([]);
		expect(refresher.state.loadError).toBe('Library is not reachable right now.');
		expect(refresher.state.loading).toBe(false);
	});

	it('counts UTF-8 bytes, not characters, on the json-only path', async () => {
		// '♪' is 1 character but 3 UTF-8 bytes: under the cap in characters,
		// well over it in bytes — the streamed-path multibyte case mirrored
		// here so a char-counting regression turns this red.
		const title = '♪'.repeat(600_000);
		const payload = JSON.stringify([{ id: 'notes', title }]);
		expect(payload.length).toBeLessThan(CAP);
		expect(new TextEncoder().encode(payload).length).toBeGreaterThan(CAP);

		const refresher = createLibraryRefresher({
			sessionStorage: () => null,
			invalidateLibraryCache: () => {},
			timeoutMs: SLOW_TIMEOUT_MS
		});

		await refresher.refresh(jsonOnlyFetch(payload));

		expect(refresher.state.items.map((i) => i.id)).toEqual([]);
		expect(refresher.state.loadError).toBe('Library is not reachable right now.');
		expect(refresher.state.loading).toBe(false);
	});

	it('parses a small multibyte json-only payload', async () => {
		const refresher = createLibraryRefresher({
			sessionStorage: () => null,
			invalidateLibraryCache: () => {},
			timeoutMs: SLOW_TIMEOUT_MS
		});

		await refresher.refresh(
			jsonOnlyFetch(JSON.stringify([{ id: 'multibyte-small', title: 'Café ♪' }]))
		);

		expect(refresher.state.items).toEqual([{ id: 'multibyte-small', title: 'Café ♪' }]);
		expect(refresher.state.loadError).toBeNull();
		expect(refresher.state.loading).toBe(false);
	});
});
