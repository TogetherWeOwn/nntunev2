import { readCappedJson } from '#lib/api/cappedJson.js';
import { captureAuthFence } from '#lib/auth-fence.js';

const PAGE_SIZE = 100;
const SHELF_SIZE = 5;
const PLAYBACK_ID = /^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/;
const MAX_UNIX_SECONDS = 253_402_300_799;

interface ListenEvent {
	key: string;
	track: string;
	event: 'start' | 'complete' | 'skip';
	recordedAt: Date;
}

export type RecentListensView =
	| { kind: 'loading' | 'empty' | 'sign-in' | 'refused' | 'error' | 'stale' }
	| { kind: 'ready'; events: ListenEvent[] };

async function readFirstPage(baseUrl: string, signal: AbortSignal): Promise<RecentListensView> {
	try {
		// A same-origin POST can send Origin: null under the document's no-referrer
		// policy. For this request only, disclose the app origin, never its URL path.
		const response = await fetch(`${baseUrl.replace(/\/+$/, '')}/v1/plays/export`, {
			method: 'POST',
			credentials: 'include',
			cache: 'no-store',
			redirect: 'error',
			referrerPolicy: 'origin',
			headers: { accept: 'application/json', 'content-type': 'application/json' },
			body: '{}',
			signal: AbortSignal.any([signal, AbortSignal.timeout(10_000)])
		});
		if (response.status !== 200) {
			void response.body?.cancel().catch(() => undefined);
			if (response.status === 401) return { kind: 'sign-in' };
			if (response.status === 403) return { kind: 'refused' };
			return { kind: 'error' };
		}
		const page = (await readCappedJson(response)) as Record<string, unknown> | null;
		if (
			page?.retention !== 'account_lifetime' ||
			!Array.isArray(page.events) || page.events.length > PAGE_SIZE ||
			!(page.next_cursor === null || (typeof page.next_cursor === 'string' && page.events.length === PAGE_SIZE))
		) return { kind: 'error' };

		const keys = new Set<string>();
		const encoder = new TextEncoder();
		const events: ListenEvent[] = [];
		for (const row of page.events) {
			const { playback_id, track_id, event, recorded_at } = (row ?? {}) as Record<string, unknown>;
			if (
				typeof playback_id !== 'string' || !PLAYBACK_ID.test(playback_id) ||
				typeof track_id !== 'string' || !track_id || encoder.encode(track_id).length > 128 ||
				(event !== 'start' && event !== 'complete' && event !== 'skip') ||
				typeof recorded_at !== 'number' || !Number.isSafeInteger(recorded_at) ||
				recorded_at < 0 || recorded_at > MAX_UNIX_SECONDS
			) return { kind: 'error' };
			const key = `${playback_id}:${event === 'start' ? 0 : 1}`;
			if (keys.has(key)) return { kind: 'error' };
			keys.add(key);
			if (events.length < SHELF_SIZE) {
				events.push({ key, track: track_id, event, recordedAt: new Date(recorded_at * 1000) });
			}
		}
		return events.length ? { kind: 'ready', events } : { kind: 'empty' };
	} catch {
		return { kind: 'error' };
	}
}

export function createRecentListensReader(baseUrl: string, publish: (view: RecentListensView) => void, capture = captureAuthFence) {
	let active: AbortController | undefined;
	let isCurrent: (() => boolean) | undefined;

	function cancel() {
		active?.abort();
		active = undefined;
		isCurrent = undefined;
	}

	function invalidate() {
		cancel();
		publish({ kind: 'stale' });
	}

	return {
		cancel,
		invalidate,
		checkCurrent() {
			if (isCurrent && !isCurrent()) invalidate();
		},
		async load() {
			cancel();
			const attempt = new AbortController();
			active = attempt;
			publish({ kind: 'loading' });
			const fence = await capture();
			// Release the shared lock after dispatch so logout can proceed while fetch is pending.
			const sent = fence && await fence.run(() => attempt.signal.aborted ? undefined : {
				result: readFirstPage(baseUrl, AbortSignal.any([attempt.signal, fence.signal]))
			});
			if (!fence || !sent) {
				if (!attempt.signal.aborted) invalidate();
				return;
			}
			const result = await sent.result;
			const published = await fence.run(() => {
				if (attempt.signal.aborted) return;
				isCurrent = fence.isCurrent;
				publish(result);
				return true;
			});
			if (!published && !attempt.signal.aborted) invalidate();
		}
	};
}
