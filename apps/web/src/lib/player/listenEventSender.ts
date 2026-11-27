import { readCappedJson } from '../api/cappedJson.js';
import { validSessionBinding, type AccountSession } from '../auth-fence.js';
import type { BrowserPlaybackSource } from './browserPlaybackGrant.js';
import { playbackIdFromGrant, type ListenEvent } from './observeListenEvents.js';

const EVENTS = '/v1/plays/events';
const REQUEST_TIMEOUT_MS = 10_000;
const RETRY_DELAYS_MS = [500, 1500];
const MAX_RETRY_AFTER_MS = 60_000;
const MAX_PLAYBACKS = 32;

type Playback = {
	trackId: string;
	isCurrent: () => boolean;
	start: Promise<boolean>;
	accepted: boolean;
	terminal?: Promise<void>;
};

interface ListenSenderOptions {
	owner: AccountSession;
	/** Recording consent for this scope. Dispose the scope when consent is withdrawn. */
	canRecord: () => boolean;
	/** The eligible non-private receipt for the exact source that started playing. */
	grantForStart: () => BrowserPlaybackSource | undefined;
	fetchFn?: typeof fetch;
}

function delay(ms: number, signal: AbortSignal): Promise<void> {
	return new Promise((resolve) => {
		if (signal.aborted) return resolve();
		const timer = setTimeout(done, ms);
		function done() {
			clearTimeout(timer);
			signal.removeEventListener('abort', done);
			resolve();
		}
		signal.addEventListener('abort', done, { once: true });
	});
}

function isTransient(status: number): boolean {
	return status === 408 || status === 425 || status === 429 || status >= 500;
}

function retryAfterMs(response: Response): number {
	const seconds = response.headers.get('retry-after')?.trim();
	return seconds && /^\d+$/.test(seconds) ? Math.min(Number(seconds) * 1000, MAX_RETRY_AFTER_MS) : 0;
}

/** Prepared browser transport. The history context and event routes are off-router. */
export function createListenEventSender({ owner, canRecord, grantForStart, fetchFn = fetch }: ListenSenderOptions) {
	const lifetime = new AbortController();
	const closing = new AbortController();
	const live = AbortSignal.any([owner.signal, lifetime.signal]);
	const open = AbortSignal.any([live, closing.signal]);
	const playbacks = new Map<string, Playback>();
	let context: string | undefined;
	let drain: Promise<void> | undefined;

	function dispose() { lifetime.abort(); }
	live.addEventListener('abort', () => {
		context = undefined;
		playbacks.clear();
	}, { once: true });

	const allowed = (signal: AbortSignal) => !signal.aborted && owner.isCurrent() && canRecord();

	async function post(path: string, signal: AbortSignal, body?: string, isCurrent?: () => boolean) {
		const requestSignal = AbortSignal.any([signal, AbortSignal.timeout(REQUEST_TIMEOUT_MS)]);
		// Fence dispatch without holding the auth lock during the network request.
		const dispatched = await owner.run(() => {
			if (!allowed(requestSignal) || isCurrent?.() === false) return;
			return { response: fetchFn(path, {
				method: 'POST', mode: 'same-origin', credentials: 'same-origin', cache: 'no-store', redirect: 'error',
				headers: body === undefined ? { accept: 'application/json' } : {
					accept: 'application/json', 'content-type': 'application/json'
				}, body, signal: requestSignal
			}).catch(() => undefined) };
		});
		if (!dispatched) return;
		const response = await dispatched.response;
		if (response?.status === 401 && allowed(requestSignal)) dispose();
		return { response, requestSignal };
	}

	async function loadContext() {
		try {
			if (!allowed(open)) return;
			const result = await post(`${EVENTS}/context`, open);
			if (!result?.response || !allowed(result.requestSignal) || result.response.status !== 200) {
				void result?.response?.body?.cancel().catch(() => undefined);
				return;
			}
			const data = await readCappedJson(result.response) as { context?: unknown } | null;
			// History context and session binding are distinct MACs with the same encoding.
			if (data && typeof data === 'object' && Object.keys(data).length === 1 && Object.hasOwn(data, 'context') &&
				allowed(result.requestSignal) && validSessionBinding(data.context) && data.context.length === 43) {
				context = data.context;
			}
		} catch { /* Missing context leaves recording unavailable. */ }
	}

	async function write(body: string, isCurrent: () => boolean, signal: AbortSignal): Promise<boolean> {
		try {
			for (let attempt = 0; allowed(signal) && isCurrent(); attempt++) {
				const result = await post(EVENTS, signal, body, isCurrent);
				if (!result) return false;
				const { response, requestSignal } = result;
				if (response?.status === 200 && allowed(requestSignal)) {
					try {
						const data = await readCappedJson(response) as { ok?: unknown } | null;
						if (!requestSignal.aborted) return data?.ok === true && Object.keys(data).length === 1 &&
							Object.hasOwn(data, 'ok') && allowed(requestSignal) && isCurrent();
					} catch (error) {
						// A broken response stream leaves the write outcome unknown. The
						// receiver accepts identical replays. Malformed JSON stays final.
						if (!(error instanceof TypeError) && !requestSignal.aborted) return false;
					}
				} else {
					void response?.body?.cancel().catch(() => undefined);
					if (response && response.status !== 200 && !isTransient(response.status)) return false;
				}
				if (!allowed(open) || !isCurrent() || attempt >= RETRY_DELAYS_MS.length) return false;
				await delay(Math.max(RETRY_DELAYS_MS[attempt], response ? retryAfterMs(response) : 0), open);
				if (open.aborted) return false;
			}
		} catch { /* Transport failure stays off the playback path. */ }
		return false;
	}

	const ready = loadContext();
	return {
		ready,
		send({ playback_id, track_id, event }: ListenEvent): void {
			const binding = context;
			if (!binding || !allowed(open) || typeof playback_id !== 'string' || playback_id.length !== 36 ||
				typeof track_id !== 'string' || !track_id || track_id.length > 128 ||
				(event !== 'start' && event !== 'complete' && event !== 'skip')) return;
			const playback = playbacks.get(playback_id);
			const body = JSON.stringify({ playback_id, track_id, event, context: binding });
			if (event === 'start') {
				if (playback || playbacks.size >= MAX_PLAYBACKS ||
					/\p{Cc}/u.test(track_id) || new TextEncoder().encode(track_id).length > 128) return;
				const grant = grantForStart();
				if (!grant?.isCurrent() || grant.trackId !== track_id || grant.playId.length !== 22 ||
					playbackIdFromGrant(grant.playId) !== playback_id) return;
				const { isCurrent } = grant;
				const entry: Playback = { trackId: track_id, isCurrent, accepted: false, start: write(body, isCurrent, open) };
				playbacks.set(playback_id, entry);
				void entry.start.then((accepted) => {
					entry.accepted = accepted;
					if (!accepted && !entry.terminal) playbacks.delete(playback_id);
				});
			} else if (playback && !playback.terminal && playback.trackId === track_id) {
				playback.terminal = playback.start.then(async (accepted) => {
					if (accepted) await write(body, playback.isCurrent, live);
					playbacks.delete(playback_id);
				});
			}
		},
		/** Stop starts and retries, and settle terminals for already accepted starts. */
		close(): Promise<void> {
			if (drain) return drain;
			closing.abort();
			drain = Promise.all([...playbacks.values()].filter((entry) => entry.accepted).map((entry) => entry.terminal))
				.then(dispose);
			return drain;
		},
		dispose
	};
}
