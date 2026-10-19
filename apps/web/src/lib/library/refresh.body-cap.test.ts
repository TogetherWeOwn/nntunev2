import { describe, expect, it, vi } from 'vitest';

import { loadSnapshot, type SnapshotStorage } from './cache.js';
import * as refreshModule from './refresh.js';
import { createLibraryRefresher, type LibraryRefresher } from './refresh.js';

/**
 * Acceptance: the library refresh JSON body is capped at 1 MiB.
 *
 * `readJsonBody` must stop reading once the accumulated bytes exceed
 * `LIBRARY_BODY_MAX_BYTES` (1 MiB), cancel the reader, and reject through
 * the refresher's existing failure shape: on a first load that is
 * `items: []` + `'Library is not reachable right now.'`, and after an
 * authenticated success it is the snapshot fallback — never the over-cap
 * payload. An over-cap body must not be fully retained: a pull-driven
 * stream proves reads stop near the cap instead of at end-of-stream.
 * Bodies at or under the cap parse exactly as before.
 *
 * The cap is counted in bytes (UTF-8), not characters. All fetches are
 * injected; no network.
 */

const CAP = 1024 * 1024; // 1 MiB — the LIBRARY_BODY_MAX_BYTES contract
const CHUNK = 64 * 1024;
const FAST_TIMEOUT_MS = 25;
const SLOW_TIMEOUT_MS = 5_000;

/** Valid library JSON of exactly `targetBytes` ASCII bytes. */
function payloadOfBytes(targetBytes: number, id = 'a'): string {
	const empty = JSON.stringify([{ id, title: '' }]);
	const pad = targetBytes - empty.length;
	if (pad < 0) throw new Error(`target ${targetBytes} below payload overhead ${empty.length}`);
	const payload = JSON.stringify([{ id, title: 'x'.repeat(pad) }]);
	if (payload.length !== targetBytes) throw new Error('payload construction drifted');
	return payload;
}

interface TrackedStream {
	stream: ReadableStream<Uint8Array>;
	cancels: number;
	/** Chunks the source actually enqueued (pull-driven, so reads drive this). */
	deliveredChunks: number;
	deliveredBytes: number;
	totalChunks: number;
}

/**
 * A pull-driven body: each `pull` enqueues the next slice of `bytes`, so
 * `deliveredChunks` shows how far the reader actually consumed. A reader
 * that stops at the cap leaves the trailing chunks undelivered.
 */
function trackedChunkedStream(
	bytes: Uint8Array,
	chunkBytes: number,
	options: { cancelError?: Error } = {}
): TrackedStream {
	let offset = 0;
	const tracked: TrackedStream = {
		stream: null as unknown as ReadableStream<Uint8Array>,
		cancels: 0,
		deliveredChunks: 0,
		deliveredBytes: 0,
		totalChunks: Math.ceil(bytes.length / chunkBytes)
	};
	tracked.stream = new ReadableStream<Uint8Array>({
		pull(controller) {
			if (offset >= bytes.length) {
				controller.close();
				return;
			}
			const slice = bytes.slice(offset, offset + chunkBytes);
			offset += slice.length;
			tracked.deliveredChunks += 1;
			tracked.deliveredBytes += slice.length;
			controller.enqueue(slice);
		},
		cancel() {
			tracked.cancels += 1;
			if (options.cancelError) throw options.cancelError;
		}
	});
	return tracked;
}

function trackedTextStream(
	text: string,
	chunkBytes: number = CHUNK,
	options: { cancelError?: Error } = {}
): TrackedStream {
	return trackedChunkedStream(new TextEncoder().encode(text), chunkBytes, options);
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

function makeRefresher(timeoutMs: number = SLOW_TIMEOUT_MS): {
	refresher: LibraryRefresher;
	storage: SnapshotStorage;
} {
	const storage = memoryStorage();
	const refresher = createLibraryRefresher({
		sessionStorage: () => storage,
		invalidateLibraryCache: () => {},
		timeoutMs
	});
	return { refresher, storage };
}

function jsonResponse(body: ReadableStream<Uint8Array> | string, status = 200): Response {
	return new Response(body, {
		status,
		headers: { 'content-type': 'application/json' }
	});
}

function fetchReturning(body: TrackedStream): typeof fetch {
	return (async () => jsonResponse(body.stream)) as typeof fetch;
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

async function expectCancelled(body: TrackedStream): Promise<void> {
	await vi.waitFor(() => expect(body.cancels).toBeGreaterThan(0), {
		timeout: 750,
		interval: 10
	});
}

function expectUnreachable(refresher: LibraryRefresher): void {
	// Compare ids, not whole items: a retained over-cap item carries a
	// megabyte title that must not flood the failure diff.
	expect(refresher.state.items.map((i) => i.id)).toEqual([]);
	expect(refresher.state.staleAt).toBeNull();
	expect(refresher.state.loadError).toBe('Library is not reachable right now.');
	expect(refresher.state.libraryUnavailable).toBe(false);
	expect(refresher.state.loading).toBe(false);
}

describe('library refresh body cap', () => {
	it('exports LIBRARY_BODY_MAX_BYTES as 1 MiB', () => {
		expect((refreshModule as Record<string, unknown>).LIBRARY_BODY_MAX_BYTES).toBe(CAP);
	});

	it('rejects a valid over-cap payload, stops reading near the cap, cancels, and saves no snapshot', async () => {
		const { refresher, storage } = makeRefresher();
		// ~3 MiB of valid JSON in 64 KiB chunks: the cap is crossed on chunk 17.
		const body = trackedTextStream(payloadOfBytes(3 * CAP, 'over'), CHUNK);

		await refresher.refresh(fetchReturning(body));

		// Existing first-load failure shape — the over-cap items never land.
		expectUnreachable(refresher);
		expect(loadSnapshot(storage)).toBeNull();
		await expectCancelled(body);
		// Reads stopped near the cap: the crossing chunk plus a few chunks
		// of stream read-ahead, far short of the whole 3 MiB body.
		expect(body.deliveredChunks).toBeLessThanOrEqual(CAP / CHUNK + 3);
		expect(body.deliveredBytes).toBeLessThanOrEqual(CAP + 3 * CHUNK);
		expect(body.deliveredChunks).toBeLessThan(body.totalChunks);

		await expectSubsequentRefreshSucceeds(refresher);
	});

	it('rejects a valid payload of exactly cap + 1 byte', async () => {
		const { refresher } = makeRefresher();
		const body = trackedTextStream(payloadOfBytes(CAP + 1, 'one-over'), CHUNK);

		await refresher.refresh(fetchReturning(body));

		expectUnreachable(refresher);

		await expectSubsequentRefreshSucceeds(refresher);
	});

	it('rejects when the first chunk alone exceeds the cap and cancels the reader', async () => {
		const { refresher } = makeRefresher();
		// First slice is 1.25 MiB and alone crosses the cap. Stream
		// read-ahead may enqueue the second slice, but the third slice
		// (carrying the JSON tail) must stay undelivered.
		const body = trackedTextStream(payloadOfBytes(3 * CAP, 'big-first'), CAP + 256 * 1024);

		await refresher.refresh(fetchReturning(body));

		expectUnreachable(refresher);
		await expectCancelled(body);
		expect(body.deliveredChunks).toBeLessThan(body.totalChunks);

		await expectSubsequentRefreshSucceeds(refresher);
	});

	it('counts UTF-8 bytes, not characters, against the cap', async () => {
		const { refresher } = makeRefresher();
		// '♪' is 1 character but 3 UTF-8 bytes: this payload is under the
		// cap in characters and well over it in bytes.
		const title = '♪'.repeat(600_000);
		const payload = JSON.stringify([{ id: 'notes', title }]);
		expect(payload.length).toBeLessThan(CAP);
		const body = trackedChunkedStream(new TextEncoder().encode(payload), CHUNK);
		expect(body.deliveredBytes).toBe(0);

		await refresher.refresh(fetchReturning(body));

		expectUnreachable(refresher);
		await expectCancelled(body);
		expect(body.deliveredBytes).toBeLessThanOrEqual(CAP + 3 * CHUNK);
		expect(body.deliveredChunks).toBeLessThan(body.totalChunks);

		await expectSubsequentRefreshSucceeds(refresher);
	});

	it('falls back to the snapshot instead of replacing items when a later refresh is over-cap', async () => {
		const { refresher } = makeRefresher();
		await refresher.refresh(okFetch([{ id: 'kept', title: 'Kept track' }]));
		expect(refresher.state.items.map((i) => i.id)).toEqual(['kept']);

		const body = trackedTextStream(payloadOfBytes(2 * CAP, 'over'), CHUNK);
		await refresher.refresh(fetchReturning(body));

		// Same failure shape as a 500/timeout after an authenticated
		// success: last-good items stay, marked stale, with no error banner.
		expect(refresher.state.items.map((i) => i.id)).toEqual(['kept']);
		expect(refresher.state.staleAt).not.toBeNull();
		expect(refresher.state.loadError).toBeNull();
		expect(refresher.state.loading).toBe(false);
		await expectCancelled(body);

		await expectSubsequentRefreshSucceeds(refresher);
	});

	it('swallows a failing reader cancel on the over-cap path and still recovers', async () => {
		const { refresher } = makeRefresher();
		const body = trackedTextStream(payloadOfBytes(2 * CAP, 'over'), CHUNK, {
			cancelError: new Error('cancel blew up')
		});

		await refresher.refresh(fetchReturning(body));

		expectUnreachable(refresher);
		// The cancel was attempted; its failure must not surface as an
		// unhandled rejection (vitest fails the file on those) or wedge state.
		await expectCancelled(body);

		await expectSubsequentRefreshSucceeds(refresher);
	});
});

describe('within-cap and adjacent refresh behavior (controls)', () => {
	it('parses a valid payload of exactly cap bytes', async () => {
		const { refresher } = makeRefresher();
		const body = trackedTextStream(payloadOfBytes(CAP, 'exact'), CHUNK);

		await refresher.refresh(fetchReturning(body));

		expect(refresher.state.items.map((i) => i.id)).toEqual(['exact']);
		expect(refresher.state.staleAt).toBeNull();
		expect(refresher.state.loadError).toBeNull();
		expect(refresher.state.loading).toBe(false);
		expect(body.deliveredChunks).toBe(body.totalChunks);
	});

	it('parses a small streamed payload split mid-character', async () => {
		const { refresher, storage } = makeRefresher();
		const bytes = new TextEncoder().encode('[{"id":"a","title":"Café ♪"}]');
		const noteStart = bytes.indexOf(0xe2); // first byte of '♪' (3-byte sequence)
		const body = trackedChunkedStream(bytes, noteStart + 1);

		await refresher.refresh(fetchReturning(body));

		expect(refresher.state.items).toEqual([{ id: 'a', title: 'Café ♪' }]);
		expect(refresher.state.loadError).toBeNull();
		expect(loadSnapshot(storage)?.items.map((i) => i.id)).toEqual(['a']);
	});

	it('accepts a small empty library', async () => {
		const { refresher } = makeRefresher();

		await refresher.refresh(okFetch([]));

		expect(refresher.state.items).toEqual([]);
		expect(refresher.state.loadError).toBeNull();
		expect(refresher.state.loading).toBe(false);
	});

	it('still treats a small invalid-shape 200 payload as unreachable', async () => {
		const { refresher } = makeRefresher();

		await refresher.refresh(okFetch({ error: 'foreign shape' }));

		expectUnreachable(refresher);
	});

	it('still times out and cancels a hanging under-cap body', async () => {
		const { refresher } = makeRefresher(FAST_TIMEOUT_MS);
		let cancels = 0;
		const hanging = new ReadableStream<Uint8Array>({
			cancel() {
				cancels += 1;
			}
		});

		await refresher.refresh((async () => jsonResponse(hanging)) as typeof fetch);

		expectUnreachable(refresher);
		await vi.waitFor(() => expect(cancels).toBeGreaterThan(0), {
			timeout: 750,
			interval: 10
		});

		await expectSubsequentRefreshSucceeds(refresher);
	});
});
