import { describe, expect, it, vi } from 'vitest';

import { SeekController, type SeekFetch } from './seekController.js';

/**
 * Bounded consumption for finite byte-range seeks.
 *
 * A finite request (`bytes=start-end`) knows exactly how many body bytes it
 * may use. The controller must therefore read the response body
 * incrementally, stop once it has (or has exceeded) that many bytes, and
 * cancel the unused remainder — instead of accumulating the whole body
 * with `response.arrayBuffer()` before validating or slicing it. These
 * fixtures observe real consumption through `pull`/`cancel` counters on a
 * synthetic `ReadableStream`; nothing here inspects source text.
 *
 * Bounds below count whole upstream chunks the source was asked to
 * produce. A single upstream chunk may cross the requested boundary (its
 * own allocation is the producer's, not the controller's), so bounds
 * allow the crossing chunk plus modest read-ahead slack, but never
 * consumption of the large synthetic remainder.
 */

const CHUNK = 64;
/** Generous read-ahead slack: 8 chunks, far below the synthetic remainders. */
const MAX_BOUNDED_BYTES = 8 * CHUNK;

function patternBytes(length: number, offset = 0): Uint8Array<ArrayBuffer> {
	return Uint8Array.from({ length }, (_, i) => (offset + i) % 251);
}

function splitChunks(bytes: Uint8Array, sizes: number[]): Uint8Array[] {
	const chunks: Uint8Array[] = [];
	let at = 0;
	for (const size of sizes) {
		if (at >= bytes.length) break;
		chunks.push(bytes.slice(at, at + size));
		at += size;
	}
	if (at < bytes.length) chunks.push(bytes.slice(at));
	return chunks;
}

function uniformChunks(bytes: Uint8Array, size = CHUNK): Uint8Array[] {
	const sizes: number[] = [];
	for (let at = 0; at < bytes.length; at += size) sizes.push(size);
	return splitChunks(bytes, sizes);
}

interface CountedBody {
	stream: ReadableStream<Uint8Array>;
	pulls: number;
	cancels: number;
	enqueuedBytes: number;
	enqueuedChunks: number;
	waitForPulls(n: number): Promise<void>;
	releaseGate(): void;
}

/**
 * A finite synthetic body with pull/cancel counters. Options can park the
 * stream after N chunks until `releaseGate()` (to model a slow body that
 * is superseded or aborted mid-read) or fault it after N chunks.
 */
function countedBody(
	chunks: Uint8Array[],
	opts: { gateAfterChunks?: number; errorAfterChunks?: number } = {}
): CountedBody {
	const waiters: Array<{ n: number; resolve: () => void }> = [];
	let index = 0;
	let cancelled = false;
	let resolveGate: () => void = () => {};
	const gate = new Promise<void>((resolve) => {
		resolveGate = resolve;
	});

	const body: CountedBody = {
		pulls: 0,
		cancels: 0,
		enqueuedBytes: 0,
		enqueuedChunks: 0,
		waitForPulls(n: number): Promise<void> {
			if (body.pulls >= n) return Promise.resolve();
			return new Promise<void>((resolve) => {
				waiters.push({ n, resolve });
			});
		},
		releaseGate(): void {
			resolveGate();
		},
		stream: undefined as unknown as ReadableStream<Uint8Array>
	};

	body.stream = new ReadableStream<Uint8Array>({
		async pull(controller) {
			body.pulls += 1;
			for (const waiter of [...waiters]) {
				if (body.pulls >= waiter.n) {
					waiters.splice(waiters.indexOf(waiter), 1);
					waiter.resolve();
				}
			}
			if (opts.errorAfterChunks !== undefined && index >= opts.errorAfterChunks) {
				controller.error(new Error('synthetic stream failure'));
				return;
			}
			if (opts.gateAfterChunks !== undefined && index === opts.gateAfterChunks) {
				await gate;
				if (cancelled) return;
			}
			if (index >= chunks.length) {
				controller.close();
				return;
			}
			const chunk = chunks[index];
			index += 1;
			body.enqueuedChunks += 1;
			body.enqueuedBytes += chunk.byteLength;
			controller.enqueue(chunk);
		},
		cancel() {
			body.cancels += 1;
			cancelled = true;
			resolveGate();
		}
	});
	return body;
}

function fetchReturning(response: Response): SeekFetch {
	return async () => response;
}

describe('SeekController bounded consumption (finite ranges)', () => {
	it('collects only the requested prefix of a large HTTP 200 and cancels the rest', async () => {
		const total = patternBytes(12_800);
		const body = countedBody(uniformChunks(total));
		const response = new Response(body.stream, { status: 200 });
		const arrayBufferSpy = vi.spyOn(response, 'arrayBuffer');
		const controller = new SeekController(
			'https://audio.example/track',
			fetchReturning(response)
		);

		const result = await controller.seek(0, 99);

		expect(result.status).toBe(200);
		expect(result.bytes).toEqual(patternBytes(100));
		expect(controller.completed).toBe(1);
		expect(controller.pending).toBe(false);
		// The finite prefix must not be collected via an unbounded body read.
		expect(arrayBufferSpy).not.toHaveBeenCalled();
		expect(body.enqueuedBytes).toBeLessThanOrEqual(MAX_BOUNDED_BYTES);
		expect(body.enqueuedBytes).toBeLessThan(total.byteLength);
		expect(body.cancels).toBeGreaterThanOrEqual(1);
	});

	it('slices a boundary-crossing chunk of an HTTP 200 without pulling later chunks', async () => {
		const total = patternBytes(4_096);
		// First chunk alone (256 bytes) crosses the 100-byte request boundary.
		const body = countedBody(splitChunks(total, [256, ...Array(60).fill(CHUNK)]));
		const controller = new SeekController(
			'https://audio.example/track',
			fetchReturning(new Response(body.stream, { status: 200 }))
		);

		const result = await controller.seek(0, 99);

		expect(result.bytes).toEqual(patternBytes(100));
		expect(controller.completed).toBe(1);
		expect(body.enqueuedChunks).toBeLessThanOrEqual(2);
		expect(body.cancels).toBeGreaterThanOrEqual(1);
	});

	it('rejects an HTTP 200 body that is shorter than the finite prefix', async () => {
		// 99 bytes cannot satisfy bytes=0-99 (100 bytes).
		const body = countedBody(uniformChunks(patternBytes(99), 33));
		const controller = new SeekController(
			'https://audio.example/track',
			fetchReturning(new Response(body.stream, { status: 200 }))
		);

		await expect(controller.seek(0, 99)).rejects.toThrow();
		expect(controller.completed).toBe(0);
		expect(controller.pending).toBe(false);
	});

	it('accepts an HTTP 200 body of exactly the finite prefix length', async () => {
		const body = countedBody(uniformChunks(patternBytes(100), 30));
		const controller = new SeekController(
			'https://audio.example/track',
			fetchReturning(new Response(body.stream, { status: 200 }))
		);

		const result = await controller.seek(0, 99);

		expect(result.bytes).toEqual(patternBytes(100));
		expect(controller.completed).toBe(1);
	});

	it('accepts an exact finite 206 delivered across boundary-crossing chunks', async () => {
		const expected = patternBytes(100, 10);
		const body = countedBody(splitChunks(expected, [30, 30, 39, 1]));
		const controller = new SeekController(
			'https://audio.example/track',
			fetchReturning(
				new Response(body.stream, {
					status: 206,
					headers: { 'content-range': 'bytes 10-109/1000' }
				})
			)
		);

		const result = await controller.seek(10, 109);

		expect(result.status).toBe(206);
		expect(result.bytes).toEqual(expected);
		expect(controller.completed).toBe(1);
		expect(body.enqueuedBytes).toBe(100);
	});

	it('rejects an oversized finite 206 without consuming the whole body', async () => {
		const total = patternBytes(12_800);
		const body = countedBody(uniformChunks(total));
		const response = new Response(body.stream, {
			status: 206,
			headers: { 'content-range': 'bytes 0-99/12800' }
		});
		const arrayBufferSpy = vi.spyOn(response, 'arrayBuffer');
		const controller = new SeekController(
			'https://audio.example/track',
			fetchReturning(response)
		);

		await expect(controller.seek(0, 99)).rejects.toThrow();

		expect(controller.completed).toBe(0);
		expect(controller.pending).toBe(false);
		expect(arrayBufferSpy).not.toHaveBeenCalled();
		expect(body.enqueuedBytes).toBeLessThanOrEqual(MAX_BOUNDED_BYTES);
		expect(body.enqueuedBytes).toBeLessThan(total.byteLength);
		expect(body.cancels).toBeGreaterThanOrEqual(1);
	});

	it('rejects a finite 206 with trailing bytes after an exact chunk boundary', async () => {
		// The first two chunks fill the 100-byte range exactly on a chunk
		// boundary, so only a post-fill probe can see the trailing 50 bytes.
		// The gate keeps the stream open after the trailing chunk so the
		// rejection's cancellation is observable (a self-closed body has
		// nothing left to cancel).
		const body = countedBody(splitChunks(patternBytes(150), [50, 50, 50]), {
			gateAfterChunks: 3
		});
		const controller = new SeekController(
			'https://audio.example/track',
			fetchReturning(
				new Response(body.stream, {
					status: 206,
					headers: { 'content-range': 'bytes 0-99/1000' }
				})
			)
		);

		await expect(controller.seek(0, 99)).rejects.toThrow();
		expect(controller.completed).toBe(0);
		expect(controller.pending).toBe(false);
		expect(body.cancels).toBeGreaterThanOrEqual(1);
	});

	it('accepts a finite 206 that ends with an empty chunk after the exact length', async () => {
		// An empty chunk carries no bytes; probing past it must not reject
		// an otherwise exact body, and a cleanly ended body needs no cancel.
		// Built explicitly: splitChunks stops once the bytes are exhausted,
		// so it would never emit the trailing empty chunk.
		const body = countedBody([patternBytes(50), patternBytes(50, 50), new Uint8Array(0)]);
		const controller = new SeekController(
			'https://audio.example/track',
			fetchReturning(
				new Response(body.stream, {
					status: 206,
					headers: { 'content-range': 'bytes 0-99/1000' }
				})
			)
		);

		const result = await controller.seek(0, 99);

		expect(result.bytes).toEqual(patternBytes(100));
		expect(controller.completed).toBe(1);
		expect(body.cancels).toBe(0);
	});

	it('rejects a finite 206 delivered as a single oversized chunk', async () => {
		// One 150-byte chunk crosses the 100-byte boundary and the stream
		// then ends, so only the boundary-overflow check can reject it.
		const body = countedBody([patternBytes(150)]);
		const controller = new SeekController(
			'https://audio.example/track',
			fetchReturning(
				new Response(body.stream, {
					status: 206,
					headers: { 'content-range': 'bytes 0-99/1000' }
				})
			)
		);

		await expect(controller.seek(0, 99)).rejects.toThrow();
		expect(controller.completed).toBe(0);
		expect(controller.pending).toBe(false);
	});

	it('rejects a finite 206 whose body ends before its Content-Range length', async () => {
		const body = countedBody(uniformChunks(patternBytes(50), 25));
		const controller = new SeekController(
			'https://audio.example/track',
			fetchReturning(
				new Response(body.stream, {
					status: 206,
					headers: { 'content-range': 'bytes 0-99/1000' }
				})
			)
		);

		await expect(controller.seek(0, 99)).rejects.toThrow();
		expect(controller.completed).toBe(0);
		expect(controller.pending).toBe(false);
	});

	it('releases the reader of a seek superseded mid-body and cannot overwrite the later seek', async () => {
		const slowBody = countedBody(splitChunks(patternBytes(100), [40, 40, 20]), {
			gateAfterChunks: 1
		});
		let calls = 0;
		const fetchFn: SeekFetch = async () => {
			calls += 1;
			if (calls === 1) {
				return new Response(slowBody.stream, {
					status: 206,
					headers: { 'content-range': 'bytes 0-99/1000' }
				});
			}
			return new Response(patternBytes(100, 200), {
				status: 206,
				headers: { 'content-range': 'bytes 200-299/1000' }
			});
		};
		const controller = new SeekController('https://audio.example/track', fetchFn);

		const first = controller.seek(0, 99);
		void first.catch(() => {});
		// First chunk consumed; the body is now parked mid-read.
		await slowBody.waitForPulls(2);

		const second = await controller.seek(200, 299);
		try {
			expect(second.bytes).toEqual(patternBytes(100, 200));
			// Supersession must cancel the parked reader, not leave it locked.
			await vi.waitFor(() => expect(slowBody.cancels).toBeGreaterThanOrEqual(1), {
				timeout: 1_000
			});
		} finally {
			slowBody.releaseGate();
		}
		await expect(first).rejects.toMatchObject({ name: 'AbortError' });

		expect(controller.completed).toBe(1);
		expect(controller.generation).toBe(2);
		expect(controller.pending).toBe(false);
	});

	it('explicit abort during body read releases the reader and the next seek works', async () => {
		const slowBody = countedBody(splitChunks(patternBytes(100), [40, 40, 20]), {
			gateAfterChunks: 1
		});
		let handler: SeekFetch = fetchReturning(
			new Response(slowBody.stream, {
				status: 206,
				headers: { 'content-range': 'bytes 0-99/1000' }
			})
		);
		const controller = new SeekController('https://audio.example/track', (url, init) =>
			handler(url, init)
		);

		const pending = controller.seek(0, 99);
		void pending.catch(() => {});
		await slowBody.waitForPulls(2);
		controller.abort();
		try {
			await vi.waitFor(() => expect(slowBody.cancels).toBeGreaterThanOrEqual(1), {
				timeout: 1_000
			});
		} finally {
			slowBody.releaseGate();
		}
		await expect(pending).rejects.toMatchObject({ name: 'AbortError' });
		expect(controller.completed).toBe(0);

		handler = fetchReturning(
			new Response(patternBytes(100), {
				status: 206,
				headers: { 'content-range': 'bytes 0-99/1000' }
			})
		);
		const after = await controller.seek(0, 99);
		expect(after.bytes).toEqual(patternBytes(100));
		expect(controller.completed).toBe(1);
		expect(controller.pending).toBe(false);
	});

	it('a body read error fails the seek and leaves the next request usable', async () => {
		const faulty = countedBody(uniformChunks(patternBytes(100), 25), {
			errorAfterChunks: 1
		});
		let handler: SeekFetch = fetchReturning(
			new Response(faulty.stream, {
				status: 206,
				headers: { 'content-range': 'bytes 0-99/1000' }
			})
		);
		const controller = new SeekController('https://audio.example/track', (url, init) =>
			handler(url, init)
		);

		await expect(controller.seek(0, 99)).rejects.toThrow();
		expect(controller.completed).toBe(0);
		expect(controller.pending).toBe(false);

		handler = fetchReturning(
			new Response(patternBytes(100), {
				status: 206,
				headers: { 'content-range': 'bytes 0-99/1000' }
			})
		);
		const after = await controller.seek(0, 99);
		expect(after.bytes).toEqual(patternBytes(100));
		expect(controller.completed).toBe(1);
	});

	it('keeps open-ended requests consuming the full validated body', async () => {
		const expected = patternBytes(100, 10);
		const body = countedBody(splitChunks(expected, [60, 40]));
		const controller = new SeekController(
			'https://audio.example/track',
			fetchReturning(
				new Response(body.stream, {
					status: 206,
					headers: { 'content-range': 'bytes 10-109/110' }
				})
			)
		);

		const result = await controller.seek(10);

		expect(result.bytes).toEqual(expected);
		expect(controller.completed).toBe(1);
		expect(body.enqueuedBytes).toBe(100);
	});
});
