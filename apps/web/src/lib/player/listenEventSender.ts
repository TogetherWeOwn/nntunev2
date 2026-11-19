/**
 * Best-effort listen-event telemetry (`POST /v1/plays/events`).
 *
 * Telemetry only reports; it never bills, never touches leases or grants and
 * never blocks, delays or errors playback. Every failure is swallowed. The
 * payload carries only the event and its opaque session binding. Pending
 * work is in memory and is dropped on `reset()` (page or session boundary).
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
	/** Drop queued/retrying events and abort in flight (account boundary). */
	reset(): void;
}

const MAX_TRACK_ID_BYTES = 128;
const DEFAULT_RETRY_DELAYS_MS = [500, 1500] as const;
const DEFAULT_TIMEOUT_MS = 10_000;
/** After a 401 stop sending; a later sign-in is picked up after this pause. */
const ANONYMOUS_COOLDOWN_MS = 5 * 60_000;
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
	fetchFn,
	retryDelaysMs = DEFAULT_RETRY_DELAYS_MS,
	timeoutMs = DEFAULT_TIMEOUT_MS
}: ListenEventSenderOptions): ListenEventSender {
	const url = `${baseUrl.replace(/\/+$/, '')}/v1/plays/events`;
	// One ordered chain per playback so a terminal event follows its start.
	// A terminal is only sent once the start was accepted (the server rejects
	// a terminal without an accepted start).
	const chains = new Map<string, { tail: Promise<void>; accepted: boolean; binding: string }>();
	let epoch = 0;
	let disabledUntil = 0;
	let controller = new AbortController();

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

	/** Resolves true when the server acknowledged the event. */
	async function attempt(
		event: ListenEvent,
		binding: string,
		myEpoch: number,
		signal: AbortSignal
	): Promise<boolean> {
		for (let n = 0; ; n++) {
			if (myEpoch !== epoch || Date.now() < disabledUntil) return false;
			let wait = 0;
			const deadline = AbortSignal.timeout(timeoutMs);
			try {
				const doFetch = fetchFn ?? ((input, init) => fetch(input, init));
				const response = await doFetch(url, {
					method: 'POST',
					credentials: 'include',
					redirect: 'error',
					keepalive: true,
					headers: { 'content-type': 'application/json', accept: 'application/json' },
					body: JSON.stringify({ ...event, context: binding }),
					signal: AbortSignal.any([signal, deadline])
				});
				if (response.ok) return myEpoch === epoch;
				if (response.status === 401) {
					// Anonymous: stop telemetry until the account boundary resets it.
					if (myEpoch === epoch) disabledUntil = Date.now() + ANONYMOUS_COOLDOWN_MS;
					return false;
				}
				if (!transient(response.status)) return false;
				wait = retryAfterMs(response);
			} catch {
				if (signal.aborted) return false;
			}
			if (n >= retryDelaysMs.length) return false;
			await sleep(Math.max(retryDelaysMs[n], wait), signal);
		}
	}

	return {
		send(event) {
			try {
				if (Date.now() < disabledUntil || !isValidTrackId(event.track_id)) return;
				const myEpoch = epoch;
				const signal = controller.signal;
				let chain = chains.get(event.playback_id);
				if (!chain) {
					if (event.event !== 'start' || chains.size >= MAX_PENDING_PLAYBACKS) return;
					const binding = context.current();
					if (!binding) return;
					chain = { tail: Promise.resolve(), accepted: false, binding };
					chains.set(event.playback_id, chain);
				}
				const owner = chain;
				const isStart = event.event === 'start';
				const next: Promise<void> = owner.tail
					.then(async () => {
						if (!isStart && !owner.accepted) return;
						const ok = await attempt(event, owner.binding, myEpoch, signal);
						if (isStart) owner.accepted = ok;
					})
					.catch(() => {})
					.then(() => {
						// Forget the playback once nothing more can follow: after its
						// terminal, or when the start was never accepted.
						if (chains.get(event.playback_id) !== owner) return;
						if (!isStart ? owner.tail === next : !owner.accepted) {
							chains.delete(event.playback_id);
						}
					});
				owner.tail = next;
			} catch {
				// Telemetry never throws into playback.
			}
		},
		reset() {
			epoch++;
			disabledUntil = 0;
			controller.abort();
			controller = new AbortController();
			chains.clear();
		}
	};
}
