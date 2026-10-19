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
});
