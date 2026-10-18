import { describe, expect, it, vi } from 'vitest';

import { SeekController, type SeekFetch } from './seekController.js';

/**
 * Pre-consumption rejection cleanup for Range seeks.
 *
 * A response can be rejected before any body bytes are consumed: an
 * HTTP error status, a `206` whose `Content-Range` is missing or does
 * not satisfy the request, a `200` answering a non-zero range, or a
 * response that only arrives after its seek was superseded. Discarding
 * such a response must release its unused body (cancel it, or abort
 * the request) so upstream work stops instead of streaming into
 * nothing. These fixtures observe real release through `pull`/`cancel`
 * counters on a synthetic `ReadableStream` and the `AbortSignal` the
 * injected fetch received; nothing here inspects source text.
 *
 * Release may be fire-and-forget, so assertions wait briefly for the
 * cancel/abort to land. A single upstream chunk may be produced by
 * read-ahead before release, so consumption bounds allow modest slack
 * but never the large synthetic remainder.
 */

const CHUNK = 64;
/** Generous read-ahead slack: 8 chunks, far below the synthetic remainders. */
const MAX_BOUNDED_BYTES = 8 * CHUNK;
const BODY_TOTAL = 12_800;

function patternBytes(length: number, offset = 0): Uint8Array<ArrayBuffer> {
	return Uint8Array.from({ length }, (_, i) => (offset + i) % 251);
}

function uniformChunks(bytes: Uint8Array, size = CHUNK): Uint8Array[] {
	const chunks: Uint8Array[] = [];
	for (let at = 0; at < bytes.length; at += size) chunks.push(bytes.slice(at, at + size));
	return chunks;
}

interface CountedBody {
	stream: ReadableStream<Uint8Array>;
	pulls: number;
	cancels: number;
	enqueuedBytes: number;
}

/**
 * A finite synthetic body with pull/cancel counters. `cancelBehavior`
 * can make the underlying cancellation itself fail, modelling a body
 * whose cleanup errors while the response is being discarded.
 */
function countedBody(
	chunks: Uint8Array[],
	opts: { cancelBehavior?: 'reject' | 'throw' } = {}
): CountedBody {
	let index = 0;
	const body: CountedBody = {
		pulls: 0,
		cancels: 0,
		enqueuedBytes: 0,
		stream: undefined as unknown as ReadableStream<Uint8Array>
	};
	body.stream = new ReadableStream<Uint8Array>({
		pull(controller) {
			body.pulls += 1;
			if (index >= chunks.length) {
				controller.close();
				return;
			}
			const chunk = chunks[index];
			index += 1;
			body.enqueuedBytes += chunk.byteLength;
			controller.enqueue(chunk);
		},
		cancel() {
			body.cancels += 1;
			if (opts.cancelBehavior === 'reject') {
				return Promise.reject(new Error('synthetic body cancel failure'));
			}
			if (opts.cancelBehavior === 'throw') {
				throw new Error('synthetic body cancel failure');
			}
			return undefined;
		}
	});
	return body;
}

function largeBody(opts: { cancelBehavior?: 'reject' | 'throw' } = {}): CountedBody {
	return countedBody(uniformChunks(patternBytes(BODY_TOTAL)), opts);
}

/** A valid 206 response satisfying `seek(start, end)` (or open-ended `seek(start)`). */
function validRangeResponse(start: number, end?: number): Response {
	const total = 262_144;
	const last = end === undefined ? total - 1 : end;
	return new Response(patternBytes(last - start + 1, start), {
		status: 206,
		headers: { 'content-range': `bytes ${start}-${last}/${total}` }
	});
}

/**
 * The discarded response was released if its body was cancelled or the
 * request signal the fetch observed was aborted (a real fetch aborts
 * the body when its signal fires). Release may settle asynchronously,
 * so wait briefly for either signal.
 */
async function expectReleased(body: CountedBody, signal: AbortSignal): Promise<void> {
	await vi.waitFor(
		() => {
			expect(
				body.cancels >= 1 || signal.aborted,
				`expected discarded body to be released (cancels=${body.cancels}, signal.aborted=${signal.aborted})`
			).toBe(true);
		},
		{ timeout: 1_000, interval: 10 }
	);
}

interface RejectionCase {
	name: string;
	seekArgs: [number, number?];
	makeResponse: (body: CountedBody) => Response;
	errorPattern: RegExp;
}

const rejectionCases: RejectionCase[] = [
	{
		name: 'HTTP 500 failure',
		seekArgs: [0, 99],
		makeResponse: (body) => new Response(body.stream, { status: 500 }),
		errorPattern: /HTTP 500/
	},
	{
		name: 'HTTP 416 failure',
		seekArgs: [200, 299],
		makeResponse: (body) => new Response(body.stream, { status: 416 }),
		errorPattern: /HTTP 416/
	},
	{
		name: '206 with a missing Content-Range',
		seekArgs: [10, 109],
		makeResponse: (body) => new Response(body.stream, { status: 206 }),
		errorPattern: /does not satisfy/
	},
	{
		name: '206 whose Content-Range starts at the wrong byte',
		seekArgs: [200, 299],
		makeResponse: (body) =>
			new Response(body.stream, {
				status: 206,
				headers: { 'content-range': 'bytes 0-99/1000' }
			}),
		errorPattern: /does not satisfy/
	},
	{
		name: 'finite 206 whose Content-Range ends at the wrong byte',
		seekArgs: [10, 109],
		makeResponse: (body) =>
			new Response(body.stream, {
				status: 206,
				headers: { 'content-range': 'bytes 10-99/1000' }
			}),
		errorPattern: /does not satisfy/
	},
	{
		name: 'open-ended 206 whose Content-Range does not reach the end of the representation',
		seekArgs: [10],
		makeResponse: (body) =>
			new Response(body.stream, {
				status: 206,
				headers: { 'content-range': 'bytes 10-99/1000' }
			}),
		errorPattern: /does not satisfy/
	},
	{
		name: 'HTTP 200 answering a finite non-zero range',
		seekArgs: [200, 299],
		makeResponse: (body) => new Response(body.stream, { status: 200 }),
		errorPattern: /non-zero byte range/
	},
	{
		name: 'HTTP 200 answering an open-ended non-zero range',
		seekArgs: [200],
		makeResponse: (body) => new Response(body.stream, { status: 200 }),
		errorPattern: /non-zero byte range/
	}
];

describe('SeekController rejected-response cleanup', () => {
	it.each(rejectionCases)(
		'releases the unused body of a rejected response: $name',
		async ({ seekArgs, makeResponse, errorPattern }) => {
			const body = largeBody();
			const response = makeResponse(body);
			const arrayBufferSpy = vi.spyOn(response, 'arrayBuffer');
			let handler: SeekFetch = async () => response;
			const signals: AbortSignal[] = [];
			const controller = new SeekController('https://audio.example/track', (url, init) => {
				signals.push(init.signal);
				return handler(url, init);
			});

			await expect(controller.seek(...seekArgs)).rejects.toThrow(errorPattern);

			expect(controller.completed).toBe(0);
			expect(controller.pending).toBe(false);
			// Rejection happens before consumption: the body must be
			// released, not drained to throw it away.
			expect(arrayBufferSpy).not.toHaveBeenCalled();
			expect(body.enqueuedBytes).toBeLessThanOrEqual(MAX_BOUNDED_BYTES);
			expect(body.enqueuedBytes).toBeLessThan(BODY_TOTAL);
			await expectReleased(body, signals[0]);
			expect(body.stream.locked).toBe(false);

			// The rejection did not wedge the controller: a valid seek
			// for the same range still succeeds.
			handler = async () => validRangeResponse(...seekArgs);
			const after = await controller.seek(...seekArgs);
			expect(after.status).toBe(206);
			expect(controller.completed).toBe(1);
			expect(controller.pending).toBe(false);
		}
	);

	it('releases a response that arrives only after its seek was superseded', async () => {
		const lateBody = largeBody();
		let resolveLateFetch: (response: Response) => void = () => {};
		const lateFetch = new Promise<Response>((resolve) => {
			resolveLateFetch = resolve;
		});
		let calls = 0;
		const fetchFn: SeekFetch = () => {
			calls += 1;
			if (calls === 1) return lateFetch;
			return Promise.resolve(validRangeResponse(200, 299));
		};
		const controller = new SeekController('https://audio.example/track', fetchFn);

		const first = controller.seek(0, 99);
		void first.catch(() => {});
		// The second seek supersedes (and aborts) the first while its
		// fetch is still in flight, and completes normally.
		const second = await controller.seek(200, 299);
		expect(second.bytes).toEqual(patternBytes(100, 200));
		expect(controller.aborted).toBe(1);
		expect(controller.completed).toBe(1);

		// Only now does the first fetch produce a response. Its signal
		// was already aborted before this body existed, so the abort
		// alone cannot release a body delivered by a fetch that ignored
		// it: the discarded body itself must be cancelled.
		resolveLateFetch(
			new Response(lateBody.stream, {
				status: 206,
				headers: { 'content-range': 'bytes 0-99/1000' }
			})
		);
		await expect(first).rejects.toMatchObject({ name: 'AbortError' });

		await vi.waitFor(() => expect(lateBody.cancels).toBeGreaterThanOrEqual(1), {
			timeout: 1_000,
			interval: 10
		});
		expect(lateBody.enqueuedBytes).toBeLessThanOrEqual(MAX_BOUNDED_BYTES);
		expect(lateBody.stream.locked).toBe(false);
		// The late response completed nothing and disturbed nothing.
		expect(controller.completed).toBe(1);
		expect(controller.generation).toBe(2);
		expect(controller.pending).toBe(false);

		const after = await controller.seek(200, 299);
		expect(after.bytes).toEqual(patternBytes(100, 200));
		expect(controller.completed).toBe(2);
	});

	it.each([
		{ name: 'rejects', cancelBehavior: 'reject' as const },
		{ name: 'throws', cancelBehavior: 'throw' as const }
	])(
		'a body cancel that $name does not replace the original response error',
		async ({ cancelBehavior }) => {
			const body = largeBody({ cancelBehavior });
			let handler: SeekFetch = async () => new Response(body.stream, { status: 500 });
			const signals: AbortSignal[] = [];
			const controller = new SeekController('https://audio.example/track', (url, init) => {
				signals.push(init.signal);
				return handler(url, init);
			});

			await expect(controller.seek(0, 99)).rejects.toThrow(/HTTP 500/);
			await expectReleased(body, signals[0]);
			// Let any late cleanup rejection settle: vitest fails the run
			// on an unhandled rejection, so reaching the next seek proves
			// the cleanup failure was contained.
			await new Promise<void>((resolve) => {
				setTimeout(resolve, 0);
			});
			expect(controller.completed).toBe(0);
			expect(controller.pending).toBe(false);
			expect(body.stream.locked).toBe(false);

			handler = async () => validRangeResponse(0, 99);
			const after = await controller.seek(0, 99);
			expect(after.bytes).toEqual(patternBytes(100));
			expect(controller.completed).toBe(1);
		}
	);

	it('rejects a body-less error response without a cleanup crash', async () => {
		let handler: SeekFetch = async () => new Response(null, { status: 500 });
		const controller = new SeekController('https://audio.example/track', (url, init) =>
			handler(url, init)
		);

		await expect(controller.seek(0, 99)).rejects.toThrow(/HTTP 500/);
		expect(controller.completed).toBe(0);
		expect(controller.pending).toBe(false);

		handler = async () => validRangeResponse(0, 99);
		const after = await controller.seek(0, 99);
		expect(after.bytes).toEqual(patternBytes(100));
		expect(controller.completed).toBe(1);
	});

	it('preserves an accepted finite 206: full body used, no cancellation of a cleanly ended body', async () => {
		const expected = patternBytes(100, 10);
		const body = countedBody(uniformChunks(expected, 30));
		const controller = new SeekController(
			'https://audio.example/track',
			async () =>
				new Response(body.stream, {
					status: 206,
					headers: { 'content-range': 'bytes 10-109/1000' }
				})
		);

		const result = await controller.seek(10, 109);

		expect(result.status).toBe(206);
		expect(result.bytes).toEqual(expected);
		expect(controller.completed).toBe(1);
		expect(body.enqueuedBytes).toBe(100);
		expect(body.cancels).toBe(0);
	});

	it('preserves an accepted open-ended 206 consuming the full validated body', async () => {
		const expected = patternBytes(100, 10);
		const body = countedBody(uniformChunks(expected, 60));
		const controller = new SeekController(
			'https://audio.example/track',
			async () =>
				new Response(body.stream, {
					status: 206,
					headers: { 'content-range': 'bytes 10-109/110' }
				})
		);

		const result = await controller.seek(10);

		expect(result.status).toBe(206);
		expect(result.bytes).toEqual(expected);
		expect(controller.completed).toBe(1);
		expect(body.enqueuedBytes).toBe(100);
	});

	it('preserves an accepted finite HTTP 200 from byte zero', async () => {
		const body = countedBody(uniformChunks(patternBytes(100), 30));
		const controller = new SeekController(
			'https://audio.example/track',
			async () => new Response(body.stream, { status: 200 })
		);

		const result = await controller.seek(0, 99);

		expect(result.status).toBe(200);
		expect(result.bytes).toEqual(patternBytes(100));
		expect(controller.completed).toBe(1);
	});
});
