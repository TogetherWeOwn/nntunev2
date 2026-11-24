import { API_REQUEST_TIMEOUT_MS, ApiClient, ApiError } from '../api/client.js';
import type { AccountSession } from '../auth-fence.js';
import { createBrowserPlaybackGrant } from './browserPlaybackGrant.js';
import type { QueuedPlaySource } from './queueController.js';

export type PrivateListeningState = { enabled: boolean; pending: boolean; feedback: string };
const PRIVATE_TTL_MS = 6 * 60 * 60 * 1000;
export function createBrowserPlaybackSession(
	owner: AccountSession, binding: string, show: (state: PrivateListeningState) => void, fetchFn?: typeof fetch
) {
	const lifetime = new AbortController();
	const signal = AbortSignal.any([owner.signal, lifetime.signal]);
	const isCurrent = () => !signal.aborted && owner.isCurrent();
	const api = new ApiClient({ baseUrl: '', fetchFn, session: { owner, binding } });
	const grant = createBrowserPlaybackGrant(owner, binding, async ({ trackId, playbackSessionId }, signal) => {
		const { status, data } = await api.writeBrowserPlayback({ track_id: trackId, session_id: playbackSessionId }, signal);
		return status === 200 ? data : undefined;
	});
	let enabled = false, uncertain = false;
	let busy: 'toggle' | 'play' | undefined;
	let expiry: ReturnType<typeof setTimeout> | undefined;
	let expired: (() => boolean) | undefined;
	function expire() {
		if (!enabled || !isCurrent()) return;
		enabled = false;
		uncertain = true;
		show({ enabled, pending: busy === 'toggle', feedback: 'Confirm private listening again to start another track.' });
	}
	return {
		owner, binding,
		dispose() { lifetime.abort(); grant.dispose(); clearTimeout(expiry); },
		async setPrivate(next: boolean) {
			if (!isCurrent() || busy) return;
			busy = 'toggle';
			show({ enabled, pending: true, feedback: '' });
			let feedback = 'Could not change private listening.';
			try {
				const wall = Date.now(), monotonic = performance.now();
				const dispatched = await owner.run(() => isCurrent() && {
					pending: api.writeBrowserPlayback({ session_id: grant.playbackSessionId, enabled: next }, signal)
						.then((reply) => ({ reply }), (error: unknown) => ({ error }))
				});
				if (!dispatched) return;
				const result = await dispatched.pending;
				if (!isCurrent()) return;
				if ('error' in result) {
					uncertain ||= !(result.error instanceof ApiError && result.error.status >= 400 && result.error.status < 500);
					return;
				}
				const { status, data } = result.reply;
				const ack = data as { session_id?: unknown; private_until?: unknown } | null;
				const advertised = Number.isSafeInteger(ack?.private_until) ? Number(ack?.private_until) * 1000 - wall : 0;
				const remaining = () => Math.min(PRIVATE_TTL_MS, advertised) - API_REQUEST_TIMEOUT_MS -
					Math.max(Date.now() - wall, performance.now() - monotonic);
				uncertain = true;
				if (status !== 200 || ack?.session_id !== grant.playbackSessionId ||
					(next ? remaining() <= 0 : ack.private_until !== null)) return;
				await owner.run(() => {
					if (!isCurrent() || (next && remaining() <= 0)) return;
					clearTimeout(expiry);
					enabled = next;
					uncertain = false;
					feedback = '';
					expired = next ? () => remaining() <= 0 : undefined;
					if (next) expiry = setTimeout(expire, remaining());
				});
			} finally {
				busy = undefined;
				if (isCurrent()) show({ enabled, pending: false, feedback });
			}
		},
		async open(trackId: string, current: () => boolean, publish: (source: QueuedPlaySource) => void) {
			if (busy || uncertain || !isCurrent()) return false;
			busy = 'play';
			try {
				return await grant.open({ trackId }, signal, () => {
					if (expired?.()) expire();
					return !uncertain && current();
				}, publish);
			} finally { busy = undefined; }
		}
	};
}
