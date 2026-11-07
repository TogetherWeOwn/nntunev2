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
	if (!Number.isFinite(start) || !Number.isSafeInteger(Math.floor(start))) {
		throw new RangeError(`Invalid seek start: ${String(start)}`);
	}
	if (end !== undefined && (!Number.isFinite(end) || !Number.isSafeInteger(Math.floor(end)))) {
		throw new RangeError(`Invalid seek end: ${String(end)}`);
	}
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

function timeoutError(): DOMException {
	return new DOMException('Seek timed out before completion', 'TimeoutError');
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
 * Race a signal-unaware body read against `signal` so the deadline (or a
 * supersede) settles even when the producer ignores the request signal.
 * Null-body fallback only: when `response.body` is `null` there is no
 * stream to cancel, so the wait is bounded here instead. Streaming bodies
 * are read through a cancellable reader (`readBody`) — racing those through
 * `response.arrayBuffer()` would lock the stream and leave a timed-out read buffering in the
 * background. On abort the read rejects with `AbortError`, which the
 * caller converts to `TimeoutError` when its own deadline fired.
 */
function raceSignalAbort<T>(read: Promise<T>, response: Response, signal: AbortSignal): Promise<T> {
	if (signal.aborted) return Promise.reject(abortError());
	let onAbort: (() => void) | undefined;
	const gate = new Promise<never>((_resolve, reject) => {
		onAbort = () => {
			let body: ReadableStream<Uint8Array> | null = null;
			try {
				body = response.body;
			} catch {
				body = null;
			}
			if (body !== null) {
				body.cancel().catch(() => undefined);
			}
			reject(abortError());
		};
		signal.addEventListener('abort', onAbort, { once: true });
	});
	return Promise.race([read, gate]).finally(() => {
		if (onAbort !== undefined) signal.removeEventListener('abort', onAbort);
	});
}

/**
 * Read a body with a cancellable reader and always release its lock.
 * Finite reads retain at most `expectedLength` bytes: exact reads probe
 * for a remainder, while prefix reads cancel it. Without finite options,
 * consume the whole stream; open-ended 206 validation stays with its caller.
 */
async function readBody(
	response: Response,
	signal: AbortSignal,
	finite?: { expectedLength: number; exact: boolean; shortMessage: string; lengthMessage: string }
): Promise<Uint8Array> {
	if (signal.aborted) throw abortError();
	if (response.body === null) {
		// No readable stream to cancel; the signal race still bounds the wait.
		const buffer = await raceSignalAbort(response.arrayBuffer(), response, signal);
		if (signal.aborted) throw abortError();
		if (finite === undefined) return new Uint8Array(buffer);
		if (buffer.byteLength < finite.expectedLength) throw new Error(finite.shortMessage);
		if (finite.exact && buffer.byteLength !== finite.expectedLength) {
			throw new Error(finite.lengthMessage);
		}
		return new Uint8Array(buffer.slice(0, finite.expectedLength));
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
		while (finite === undefined || collected < finite.expectedLength) {
			const { done, value } = await reader.read();
			if (signal.aborted) throw abortError();
			if (done) {
				streamDone = true;
				break;
			}
			if (value === undefined || value.byteLength === 0) continue;
			if (finite === undefined) {
				parts.push(value);
				collected += value.byteLength;
			} else {
				const remaining = finite.expectedLength - collected;
				if (value.byteLength > remaining) overflow = true;
				// Copy only up to the boundary, never retaining its excess.
				parts.push(value.slice(0, remaining));
				collected += Math.min(value.byteLength, remaining);
			}
		}
		if (finite !== undefined) {
			if (signal.aborted) throw abortError();
			if (collected < finite.expectedLength) throw new Error(finite.shortMessage);
			if (finite.exact) {
				if (overflow) throw new Error(finite.lengthMessage);
				if (!streamDone) {
					// Empty chunks carry no bytes; probe past them for a remainder.
					for (;;) {
						const probe = await reader.read();
						if (signal.aborted) throw abortError();
						if (probe.done) {
							streamDone = true;
							break;
						}
						if (probe.value !== undefined && probe.value.byteLength > 0) {
							throw new Error(finite.lengthMessage);
						}
					}
				}
			}
		}
		const bytes = new Uint8Array(collected);
		let offset = 0;
		for (const part of parts) {
			bytes.set(part, offset);
			offset += part.byteLength;
		}
		return bytes;
	} finally {
		signal.removeEventListener('abort', onAbort);
		if (!streamDone) {
			// Prefix satisfied, body rejected, or seek aborted: stop the producer.
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
	private deadlineMs?: number;

	/** Seeks started (monotonic). */
	generation = 0;
	/** Predecessor requests aborted. */
	aborted = 0;
	/** Requests that completed with a validated range body. */
	completed = 0;

	constructor(url: string, fetchFn?: SeekFetch, options?: { deadlineMs?: number }) {
		this.url = url;
		this.fetchFn =
			fetchFn ??
			((requestUrl, init) => fetch(requestUrl, init as RequestInit) as Promise<Response>);
		const deadlineMs = options?.deadlineMs;
		if (deadlineMs !== undefined) {
			// A computed-but-invalid deadline (NaN or ±Infinity, often parsed
			// from config) must fail loudly: silently running without a timer
			// would reintroduce exactly the hangs this deadline closes.
			// Negative deadlines clamp to zero (fire immediately), mirroring
			// how buildRangeHeader clamps negative positions after validation.
			if (!Number.isFinite(deadlineMs)) {
				throw new RangeError(`Invalid seek deadlineMs: ${String(deadlineMs)}`);
			}
			this.deadlineMs = Math.max(0, deadlineMs);
		}
	}

	get pending(): boolean {
		return this.current !== null;
	}

	/**
	 * Seek to `[start, end]`. Aborts any in-flight seek, then issues a
	 * `Range` request. Throws fetch and invalid-response errors, including
	 * `AbortError` when this seek is itself superseded. When a
	 * constructor-injected `deadlineMs` elapses before the seek settles,
	 * the request signal is aborted, any unread body is released, and the
	 * seek rejects with a `TimeoutError` DOMException.
	 */
	async seek(start: number, end?: number): Promise<SeekResult> {
		// Validate before any state change or fetch so non-finite
		// positions never abort in-flight work nor reach the wire.
		const rangeHeader = buildRangeHeader(start, end);
		this.abortInFlight();
		const controller = new AbortController();
		this.current = controller;
		this.generation += 1;
		const generation = this.generation;
		const requestedStart = Number(/^bytes=(\d+)-/.exec(rangeHeader)?.[1]);
		const requestedEndText = /-(\d+)$/.exec(rangeHeader)?.[1];
		const requestedEnd = requestedEndText === undefined ? undefined : Number(requestedEndText);
		// Deadline: abort only this seek's signal. A superseded seek already
		// has an aborted signal, so the timer yields to the supersede path
		// and never converts an AbortError into a TimeoutError.
		let timeoutId: ReturnType<typeof setTimeout> | undefined;
		let timedOut = false;
		// The constructor rejects non-finite deadlines, so any defined value
		// here is a real timer. The clamp stays as defense for a zero delay.
		const deadlineMs = this.deadlineMs;
		if (deadlineMs !== undefined) {
			timeoutId = setTimeout(
				() => {
					if (controller.signal.aborted) return;
					timedOut = true;
					controller.abort();
				},
				Math.max(0, deadlineMs)
			);
		}
		const throwSupersededOrTimeout = (): never => {
			if (timedOut) throw timeoutError();
			throw abortError();
		};
		try {
			let response: Response;
			try {
				response = await this.fetchFn(this.url, {
					headers: { range: rangeHeader },
					signal: controller.signal
				});
			} catch (error) {
				if (timedOut) throw timeoutError();
				throw error;
			}
			if (controller.signal.aborted) {
				await releaseRejectedBody(response);
				throwSupersededOrTimeout();
			}

			if (response.status !== 206 && response.status !== 200) {
				await releaseRejectedBody(response);
				if (timedOut) throw timeoutError();
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
					if (timedOut) throw timeoutError();
					throw new Error('Range response does not satisfy the requested byte range');
				}
				const expectedLength = range.end - range.start + 1;
				let bytes: Uint8Array;
				try {
					bytes =
						requestedEnd === undefined
							? await this.readOpenEnded206(response, expectedLength, controller.signal)
							: await readBody(response, controller.signal, {
									expectedLength,
									exact: true,
									shortMessage: 'Range response body length does not match Content-Range',
									lengthMessage: 'Range response body length does not match Content-Range'
								});
				} catch (error) {
					// Body readers cancel their reader and release the lock
					// in their finally; surface the timeout identity.
					if (timedOut) throw timeoutError();
					throw error;
				}
				if (controller.signal.aborted) throwSupersededOrTimeout();
				this.completed += 1;
				return { status: response.status, bytes, generation };
			}

			// A 200 is a full representation, so it only satisfies a range
			// starting at byte zero. Slice only when the caller asked for an end.
			if (requestedStart !== 0) {
				await releaseRejectedBody(response);
				if (timedOut) throw timeoutError();
				throw new Error('Server ignored a non-zero byte range');
			}
			if (requestedEnd !== undefined) {
				// Finite prefix: collect only the requested bytes and cancel
				// the unused remainder instead of buffering the whole body.
				let bytes: Uint8Array;
				try {
					bytes = await readBody(response, controller.signal, {
						expectedLength: requestedEnd + 1,
						exact: false,
						shortMessage: 'Full response is shorter than the requested byte range',
						lengthMessage: 'Full response is shorter than the requested byte range'
					});
				} catch (error) {
					if (timedOut) throw timeoutError();
					throw error;
				}
				if (controller.signal.aborted) throwSupersededOrTimeout();
				this.completed += 1;
				return { status: response.status, bytes, generation };
			}
			let bytes: Uint8Array;
			try {
				bytes = await readBody(response, controller.signal);
			} catch (error) {
				if (timedOut) throw timeoutError();
				throw error;
			}
			if (controller.signal.aborted) throwSupersededOrTimeout();
			this.completed += 1;
			return { status: response.status, bytes, generation };
		} finally {
			if (timeoutId !== undefined) clearTimeout(timeoutId);
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
		// Reader-based, not `arrayBuffer()`: the reader stays cancellable so
		// a deadline aborts the stalled body instead of leaving it buffering.
		const bytes = await readBody(response, signal);
		if (bytes.byteLength !== expectedLength) {
			throw new Error('Range response body length does not match Content-Range');
		}
		if (signal.aborted) throw abortError();
		return bytes;
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
