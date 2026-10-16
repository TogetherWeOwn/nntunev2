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
				throw new DOMException('Seek superseded by a newer seek', 'AbortError');
			}

			if (response.status !== 206 && response.status !== 200) {
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
					throw new Error('Range response does not satisfy the requested byte range');
				}
				const buffer = await response.arrayBuffer();
				if (buffer.byteLength !== range.end - range.start + 1) {
					throw new Error('Range response body length does not match Content-Range');
				}
				if (controller.signal.aborted) {
					throw new DOMException('Seek superseded by a newer seek', 'AbortError');
				}
				this.completed += 1;
				return { status: response.status, bytes: new Uint8Array(buffer), generation };
			}

			// A 200 is a full representation, so it only satisfies a range
			// starting at byte zero. Slice only when the caller asked for an end.
			if (requestedStart !== 0) throw new Error('Server ignored a non-zero byte range');
			const buffer = await response.arrayBuffer();
			if (requestedEnd !== undefined && buffer.byteLength <= requestedEnd) {
				throw new Error('Full response is shorter than the requested byte range');
			}
			if (controller.signal.aborted) {
				throw new DOMException('Seek superseded by a newer seek', 'AbortError');
			}
			this.completed += 1;
			const bytes = new Uint8Array(buffer);
			return {
				status: response.status,
				bytes: requestedEnd === undefined ? bytes : bytes.slice(0, requestedEnd + 1),
				generation
			};
		} finally {
			if (this.current === controller) {
				this.current = null;
			}
		}
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
