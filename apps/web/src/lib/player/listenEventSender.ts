/**
 * Best-effort listen-event telemetry (`POST /v1/plays/events`).
 *
 * Pending work lives in memory and remains bound to the original playback session.
 */

import type { ListenContext } from './listenContext.js';

export type ListenEventName = 'start' | 'complete' | 'skip';

export interface ListenEvent {
	playback_id: string;
	track_id: string;
	event: ListenEventName;
}

export interface ListenEventSenderOptions {
	baseUrl: string;
	context: Pick<ListenContext, 'current'>;
	fetchFn?: typeof fetch;
	/** Delays between attempts; its length bounds the retries. */
	retryDelaysMs?: readonly number[];
	/** Per-request deadline. */
	timeoutMs?: number;
}

export interface ListenEventSender {
	send(event: ListenEvent): void;
	/** Reject new work and finish queued first attempts without retrying. */
	close(): Promise<void>;
	/** Permanently stop sending and abort pending work. */
	dispose(): void;
}

const MAX_TRACK_ID_BYTES = 128;
const DEFAULT_RETRY_DELAYS_MS = [500, 1500] as const;
const DEFAULT_TIMEOUT_MS = 10_000;
/** In-memory ceiling on playbacks with unsent events; excess is dropped. */
const MAX_PENDING_PLAYBACKS = 32;
/** Longest Retry-After honoured; the server asks for 60 s on 429. */
const MAX_RETRY_AFTER_MS = 60_000;

export function isValidTrackId(trackId: string): boolean {
	// Non-empty, at most 128 UTF-8 bytes, no control characters (C0, DEL, C1).
	if (trackId.length === 0 || /\p{Cc}/u.test(trackId)) return false;
	return new TextEncoder().encode(trackId).length <= MAX_TRACK_ID_BYTES;
}

function transient(status: number): boolean {
	return status === 408 || status === 425 || status === 429 || status >= 500;
}

/** Retry-After in whole seconds only, capped so a retry stays bounded. */
function retryAfterMs(response: Response): number {
	const seconds = Number(response.headers?.get('retry-after'));
	if (!Number.isFinite(seconds) || seconds <= 0) return 0;
	return Math.min(seconds * 1000, MAX_RETRY_AFTER_MS);
}

export function createListenEventSender({
	baseUrl,
	context,
	fetchFn = fetch,
	retryDelaysMs = DEFAULT_RETRY_DELAYS_MS,
	timeoutMs = DEFAULT_TIMEOUT_MS
}: ListenEventSenderOptions): ListenEventSender {
	const url = `${baseUrl.replace(/\/+$/, '')}/v1/plays/events`;
	// Ordered chain and immutable binding for each playback.
	const chains = new Map<string, { tail?: Promise<void>; accepted: boolean; binding: string }>();
	const controller = new AbortController();
	const retries = new AbortController();
	const { signal } = controller;
	function dispose(): void {
		controller.abort();
		retries.abort();
		chains.clear();
	}

	async function close(): Promise<void> {
		retries.abort();
		await Promise.all([...chains.values()].map(({ tail }) => tail));
		dispose();
	}

	function sleep(ms: number, signal: AbortSignal): Promise<void> {
		return new Promise((resolve) => {
			if (signal.aborted) return resolve();
			const done = () => {
				clearTimeout(timer);
				signal.removeEventListener('abort', done);
				resolve();
			};
			const timer = setTimeout(done, ms);
			signal.addEventListener('abort', done, { once: true });
		});
	}

	async function attempt(event: ListenEvent, binding: string): Promise<boolean> {
		for (let n = 0; !signal.aborted; n++) {
			let wait = 0;
			try {
				const response = await fetchFn(url, {
					method: 'POST',
					credentials: 'include',
					redirect: 'error',
					keepalive: true,
					headers: { 'content-type': 'application/json', accept: 'application/json' },
					body: JSON.stringify({ ...event, context: binding }),
					signal: AbortSignal.any([signal, AbortSignal.timeout(timeoutMs)])
				});
				if (response.ok) return !signal.aborted;
				if (response.status === 401) dispose();
				if (!transient(response.status)) return false;
				wait = retryAfterMs(response);
			} catch {
				// Network failure: retry within the same scope and budget.
			}
			if (retries.signal.aborted || n >= retryDelaysMs.length) break;
			await sleep(Math.max(retryDelaysMs[n], wait), retries.signal);
			if (retries.signal.aborted) break;
		}
		return false;
	}

	return {
		send(event) {
			try {
				if (retries.signal.aborted || !isValidTrackId(event.track_id)) return;
				let chain = chains.get(event.playback_id);
				if (!chain) {
					if (event.event !== 'start' || chains.size >= MAX_PENDING_PLAYBACKS) return;
					const binding = context.current();
					if (!binding) return;
					chain = { accepted: false, binding };
					chains.set(event.playback_id, chain);
				}
				const owner = chain;
				const isStart = event.event === 'start';
				const send = async () => {
					if (!isStart && !owner.accepted) return;
					const ok = await attempt(event, owner.binding);
					if (isStart) owner.accepted = ok;
				};
				// Dispatch ready keepalive fetches before pagehide returns.
				const next: Promise<void> = (owner.tail ? owner.tail.then(send) : send())
					.catch(() => {})
					.then(() => {
						if (chains.get(event.playback_id) !== owner) return;
						if (isStart && !owner.accepted) chains.delete(event.playback_id);
						else if (owner.tail === next && isStart) delete owner.tail;
						else if (owner.tail === next) chains.delete(event.playback_id);
					});
				owner.tail = next;
			} catch {
				// Telemetry never throws into playback.
			}
		},
		close,
		dispose
	};
}
