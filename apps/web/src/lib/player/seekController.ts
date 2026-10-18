/**
 * Abortable Range seeks: every new seek aborts the in-flight request
 * first, so a storm of rapid seeks never wedges the player. Only the
 * latest request's bytes are used; aborted predecessors reject with
 * an `AbortError` and free their controller.
 */

export type SeekFetch = (
	url: string,
	init: { headers: Record<string, string>; signal: AbortSignal }
) => Promise<Response>;

export interface SeekResult {
	status: number;
	bytes: Uint8Array;
	generation: number;
}

export function buildRangeHeader(start: number, end?: number): string {
	const safeStart = Math.max(0, Math.floor(start));
	if (end === undefined) return `bytes=${safeStart}-`;
	const safeEnd = Math.max(safeStart, Math.floor(end));
	return `bytes=${safeStart}-${safeEnd}`;
}

function parseContentRange(value: string | null): { start: number; end: number; total: number } | null {
	if (value === null) return null;
	const match = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(value);
	if (!match) return null;
	const [, startText, endText, totalText] = match;
	const start = Number(startText);
	const end = Number(endText);
	const total = Number(totalText);
	if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || !Number.isSafeInteger(total)) {
		return null;
	}
	if (start > end || end >= total) return null;
	return { start, end, total };
}

function abortError(): DOMException {
	return new DOMException('Seek superseded by a newer seek', 'AbortError');
}

/**
 * Release a response discarded before body consumption. A rejected
 * streaming response must not keep producing into nothing: cancel its
 * unused body so upstream work stops. Cleanup failures are swallowed
 * so they never replace the original response error, and a missing
 * body is a no-op. Never reads the body (no `arrayBuffer`) and never
 * retains a reader lock.
 */
async function releaseRejectedBody(response: Response): Promise<void> {
	let body: ReadableStream<Uint8Array> | null;
	try {
		body = response.body;
	} catch {
		return;
	}
	if (body === null) return;
	try {
		await body.cancel();
	} catch {
		// Cleanup failure contained; caller still throws the original error.
	}
}

/**
 * Read at most `expectedLength` body bytes incrementally, so a finite
 * range never accumulates an oversized response before validation.
 * Accumulated buffers are bounded by `expectedLength` (a boundary
 * chunk is copied only up to the boundary); a single upstream chunk's
 * own allocation remains the producer's. With `exact`, a body longer
 * or shorter than `expectedLength` is rejected; without it, only the
 * prefix is collected and the unused remainder is cancelled. Aborting
 * `signal` cancels the reader, and the reader is always released.
 */
async function readFiniteBody(
	response: Response,
	expectedLength: number,
	signal: AbortSignal,
	options: { exact: boolean; shortMessage: string; lengthMessage: string }
): Promise<Uint8Array> {
	if (signal.aborted) throw abortError();
	if (response.body === null) {
		// No readable stream to bound; fall back to a length-checked read.
		const buffer = await response.arrayBuffer();
		if (signal.aborted) throw abortError();
		if (buffer.byteLength < expectedLength) throw new Error(options.shortMessage);
		if (options.exact && buffer.byteLength !== expectedLength) {
			throw new Error(options.lengthMessage);
		}
		return new Uint8Array(buffer.slice(0, expectedLength));
	}

	const reader = response.body.getReader();
	const onAbort = () => {
		void reader.cancel().catch(() => undefined);
	};
	signal.addEventListener('abort', onAbort, { once: true });

	const parts: Uint8Array[] = [];
	let collected = 0;
	let overflow = false;
	let streamDone = false;
	try {
		while (collected < expectedLength) {
			const { done, value } = await reader.read();
			if (signal.aborted) throw abortError();
			if (done) {
				streamDone = true;
				break;
			}
			if (value === undefined || value.byteLength === 0) continue;
			const remaining = expectedLength - collected;
			if (value.byteLength > remaining) overflow = true;
			// Copy only up to the boundary so we never retain the excess
			// of a boundary-crossing chunk.
			parts.push(value.slice(0, remaining));
			collected += Math.min(value.byteLength, remaining);
		}
		if (signal.aborted) throw abortError();
		if (collected < expectedLength) throw new Error(options.shortMessage);
		if (options.exact) {
			if (overflow) throw new Error(options.lengthMessage);
			if (!streamDone) {
				// Exactly full: probe reads distinguish an exact body from
				// one with an unrequested remainder. Empty chunks carry no
				// bytes, so probe past them like the main loop does.
				for (;;) {
					const probe = await reader.read();
					if (signal.aborted) throw abortError();
					if (probe.done) {
						streamDone = true;
						break;
					}
					if (probe.value !== undefined && probe.value.byteLength > 0) {
						throw new Error(options.lengthMessage);
					}
				}
			}
		}
		const bytes = new Uint8Array(expectedLength);
		let offset = 0;
		for (const part of parts) {
			bytes.set(part, offset);
			offset += part.byteLength;
		}
		return bytes;
	} finally {
		signal.removeEventListener('abort', onAbort);
		if (!streamDone) {
			// Prefix satisfied, body rejected, or seek aborted: cancel
			// whatever remains so the producer stops and nothing leaks.
			await reader.cancel().catch(() => undefined);
		}
		try {
			reader.releaseLock();
		} catch {
			// Already released; nothing to free.
		}
	}
}

export class SeekController {
	private current: AbortController | null = null;
	private fetchFn: SeekFetch;
	readonly url: string;

	/** Seeks started (monotonic). */
	generation = 0;
	/** Predecessor requests aborted. */
	aborted = 0;
	/** Requests that completed with a validated range body. */
	completed = 0;

	constructor(url: string, fetchFn?: SeekFetch) {
		this.url = url;
		this.fetchFn =
			fetchFn ??
			((requestUrl, init) => fetch(requestUrl, init as RequestInit) as Promise<Response>);
	}

	get pending(): boolean {
		return this.current !== null;
	}

	/**
	 * Seek to `[start, end]`. Aborts any in-flight seek, then issues a
	 * `Range` request. Throws fetch and invalid-response errors, including
	 * `AbortError` when this seek is itself superseded.
	 */
	async seek(start: number, end?: number): Promise<SeekResult> {
		this.abortInFlight();
		const controller = new AbortController();
		this.current = controller;
		this.generation += 1;
		const generation = this.generation;
		const rangeHeader = buildRangeHeader(start, end);
		const requestedStart = Number(/^bytes=(\d+)-/.exec(rangeHeader)?.[1]);
		const requestedEndText = /-(\d+)$/.exec(rangeHeader)?.[1];
		const requestedEnd = requestedEndText === undefined ? undefined : Number(requestedEndText);
		try {
			const response = await this.fetchFn(this.url, {
				headers: { range: rangeHeader },
				signal: controller.signal
			});
			if (controller.signal.aborted) {
				await releaseRejectedBody(response);
				throw new DOMException('Seek superseded by a newer seek', 'AbortError');
			}

			if (response.status !== 206 && response.status !== 200) {
				await releaseRejectedBody(response);
				throw new Error(`Range request failed with HTTP ${response.status}`);
			}
			if (response.status === 206) {
				const range = parseContentRange(response.headers.get('content-range'));
				if (
					range === null ||
					range.start !== requestedStart ||
					(requestedEnd !== undefined && range.end !== Math.min(requestedEnd, range.total - 1)) ||
					(requestedEnd === undefined && range.end !== range.total - 1)
				) {
					await releaseRejectedBody(response);
					throw new Error('Range response does not satisfy the requested byte range');
				}
				const expectedLength = range.end - range.start + 1;
				const bytes =
					requestedEnd === undefined
						? await this.readOpenEnded206(response, expectedLength, controller.signal)
						: await readFiniteBody(response, expectedLength, controller.signal, {
								exact: true,
								shortMessage: 'Range response body length does not match Content-Range',
								lengthMessage: 'Range response body length does not match Content-Range'
							});
				if (controller.signal.aborted) throw abortError();
				this.completed += 1;
				return { status: response.status, bytes, generation };
			}

			// A 200 is a full representation, so it only satisfies a range
			// starting at byte zero. Slice only when the caller asked for an end.
			if (requestedStart !== 0) {
				await releaseRejectedBody(response);
				throw new Error('Server ignored a non-zero byte range');
			}
			if (requestedEnd !== undefined) {
				// Finite prefix: collect only the requested bytes and cancel
				// the unused remainder instead of buffering the whole body.
				const bytes = await readFiniteBody(response, requestedEnd + 1, controller.signal, {
					exact: false,
					shortMessage: 'Full response is shorter than the requested byte range',
					lengthMessage: 'Full response is shorter than the requested byte range'
				});
				if (controller.signal.aborted) throw abortError();
				this.completed += 1;
				return { status: response.status, bytes, generation };
			}
			const buffer = await response.arrayBuffer();
			if (controller.signal.aborted) throw abortError();
			this.completed += 1;
			return { status: response.status, bytes: new Uint8Array(buffer), generation };
		} finally {
			if (this.current === controller) {
				this.current = null;
			}
		}
	}

	/** Open-ended policy is unchanged: consume and validate the full body. */
	private async readOpenEnded206(
		response: Response,
		expectedLength: number,
		signal: AbortSignal
	): Promise<Uint8Array> {
		const buffer = await response.arrayBuffer();
		if (buffer.byteLength !== expectedLength) {
			throw new Error('Range response body length does not match Content-Range');
		}
		if (signal.aborted) throw abortError();
		return new Uint8Array(buffer);
	}

	/** Abort the in-flight seek, if any. Never wedges: the next seek works. */
	abort(): void {
		this.abortInFlight();
	}

	private abortInFlight(): void {
		if (this.current !== null) {
			this.current.abort();
			this.current = null;
			this.aborted += 1;
		}
	}
}
