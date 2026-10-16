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

export class SeekController {
	private current: AbortController | null = null;
	private fetchFn: SeekFetch;
	readonly url: string;

	/** Seeks started (monotonic). */
	generation = 0;
	/** Predecessor requests aborted. */
	aborted = 0;
	/** Requests that completed with a body. */
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
	 * `Range` request. Throws the fetch error (including `AbortError`
	 * when this seek is itself superseded).
	 */
	async seek(start: number, end?: number): Promise<SeekResult> {
		this.abortInFlight();
		const controller = new AbortController();
		this.current = controller;
		this.generation += 1;
		const generation = this.generation;
		try {
			const response = await this.fetchFn(this.url, {
				headers: { range: buildRangeHeader(start, end) },
				signal: controller.signal
			});
			if (controller.signal.aborted) {
				throw new DOMException('Seek superseded by a newer seek', 'AbortError');
			}
			const buffer = await response.arrayBuffer();
			if (controller.signal.aborted) {
				throw new DOMException('Seek superseded by a newer seek', 'AbortError');
			}
			this.completed += 1;
			return { status: response.status, bytes: new Uint8Array(buffer), generation };
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
