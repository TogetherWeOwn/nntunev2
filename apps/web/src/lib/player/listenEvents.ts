/**
 * Best-effort listen-event telemetry (`POST /v1/plays/events`).
 *
 * Telemetry only reports; it never bills, never touches leases or grants and
 * never blocks, delays or errors playback. Every failure is swallowed. The
 * payload is `{ playback_id, track_id, event }` only: no URL, no credential.
 * Nothing is persisted: pending work lives in memory, is bounded by a small
 * retry budget, and is dropped on `reset()` (account change) so an event is
 * never uploaded under a different account.
 */

export type ListenEventName = 'start' | 'complete' | 'skip';

export interface ListenEvent {
	playback_id: string;
	track_id: string;
	event: ListenEventName;
}

export interface ListenEventSenderOptions {
	baseUrl: string;
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
	fetchFn,
	retryDelaysMs = DEFAULT_RETRY_DELAYS_MS,
	timeoutMs = DEFAULT_TIMEOUT_MS
}: ListenEventSenderOptions): ListenEventSender {
	const url = `${baseUrl.replace(/\/+$/, '')}/v1/plays/events`;
	// One ordered chain per playback so a terminal event follows its start.
	// A terminal is only sent once the start was accepted (the server rejects
	// a terminal without an accepted start).
	const chains = new Map<string, { tail: Promise<void>; accepted: boolean }>();
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
	async function attempt(event: ListenEvent, myEpoch: number, signal: AbortSignal): Promise<boolean> {
		for (let n = 0; ; n++) {
			if (myEpoch !== epoch || Date.now() < disabledUntil) return false;
			let wait = 0;
			const deadline = AbortSignal.timeout(timeoutMs);
			try {
				const doFetch = fetchFn ?? ((input, init) => fetch(input, init));
				const response = await doFetch(url, {
					method: 'POST',
					credentials: 'include',
					keepalive: true,
					headers: { 'content-type': 'application/json', accept: 'application/json' },
					body: JSON.stringify(event),
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
					chain = { tail: Promise.resolve(), accepted: false };
					chains.set(event.playback_id, chain);
				}
				const owner = chain;
				const isStart = event.event === 'start';
				const next: Promise<void> = owner.tail
					.then(async () => {
						if (!isStart && !owner.accepted) return;
						const ok = await attempt(event, myEpoch, signal);
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

/** The slice of the audio element the observer reads. */
export interface ListenAudioLike {
	readonly ended?: boolean;
	addEventListener(name: string, listener: () => void): void;
	removeEventListener(name: string, listener: () => void): void;
}

export interface ListenObserverOptions {
	/** Track currently loaded in the element; read when playback actually starts. */
	trackId: () => string | undefined;
	sender: Pick<ListenEventSender, 'send'>;
	newId?: () => string;
}

export interface ListenObserver {
	/** Release listeners; a started playback ends (complete if the media ended, else skip). */
	dispose(): void;
	/** Account boundary: forget the active playback without emitting anything. */
	reset(): void;
}

/** RFC 4122 v4 id; `randomUUID` is absent in insecure contexts. */
export function newPlaybackId(): string {
	if (typeof crypto.randomUUID === 'function') return crypto.randomUUID();
	const b = crypto.getRandomValues(new Uint8Array(16));
	b[6] = (b[6] & 0x0f) | 0x40;
	b[8] = (b[8] & 0x3f) | 0x80;
	const h = [...b].map((x) => x.toString(16).padStart(2, '0')).join('');
	return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

/**
 * Observe one audio element. `start` fires on the first actual `playing`
 * of a playback; resume after pause or buffering is the same playback.
 * `complete` fires on natural end (a repeat restart then gets a fresh
 * playback id). `skip` fires when a started playback is abandoned: source
 * emptied, element released or the observer disposed. A playback that never
 * started (failed load) emits nothing.
 *
 * Page `onended` handlers can swap the source and tear this element down
 * before our own `ended` listener runs (microtasks run between listeners).
 * So a natural end is also recognised from `element.ended`, sampled on
 * `pause`/`timeupdate` too, because teardown clears it before `emptied`.
 *
 * Every callback is contained: a failing sender or id source never reaches
 * the element's event dispatch or its teardown.
 */
export function observeListenEvents(
	element: ListenAudioLike,
	{ trackId, sender, newId = newPlaybackId }: ListenObserverOptions
): ListenObserver {
	let active: { id: string; track: string } | null = null;
	let endedSeen = false;

	function emit(playback_id: string, track_id: string, event: ListenEvent['event']): void {
		try {
			sender.send({ playback_id, track_id, event });
		} catch {
			// Best effort.
		}
	}

	function finish(event: 'complete' | 'skip'): void {
		const playback = active;
		active = null;
		endedSeen = false;
		if (playback) emit(playback.id, playback.track, event);
	}

	function finishAbandoned(): void {
		let ended = endedSeen;
		try {
			ended = ended || element.ended === true;
		} catch {
			// Fall back to what was sampled earlier.
		}
		finish(ended ? 'complete' : 'skip');
	}

	function safe(fn: () => void): () => void {
		return () => {
			try {
				fn();
			} catch {
				// Never error playback.
			}
		};
	}

	const onPlaying = safe(() => {
		endedSeen = false;
		if (active) return;
		const track = trackId();
		if (!track || !isValidTrackId(track)) return;
		active = { id: newId(), track };
		emit(active.id, track, 'start');
	});
	const sampleEnded = safe(() => {
		if (element.ended === true) endedSeen = true;
	});
	const onSeeking = safe(() => {
		endedSeen = false;
	});
	const onEnded = safe(() => finish('complete'));
	const onEmptied = safe(finishAbandoned);
	const listeners: [string, () => void][] = [
		['playing', onPlaying],
		['pause', sampleEnded],
		['timeupdate', sampleEnded],
		['seeking', onSeeking],
		['ended', onEnded],
		['emptied', onEmptied]
	];
	for (const [name, listener] of listeners) element.addEventListener(name, listener);
	let disposed = false;
	return {
		dispose: safe(() => {
			if (disposed) return;
			disposed = true;
			for (const [name, listener] of listeners) element.removeEventListener(name, listener);
			finishAbandoned();
		}),
		reset() {
			active = null;
			endedSeen = false;
		}
	};
}
