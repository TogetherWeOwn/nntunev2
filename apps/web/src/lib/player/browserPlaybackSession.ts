import { ApiClient, ApiError } from '../api/client.js';
import type { AccountSession } from '../auth-fence.js';
import { createBrowserPlaybackGrant } from './browserPlaybackGrant.js';
import type { QueuedPlaySource } from './queueController.js';

export type PrivateListeningState = { enabled: boolean; pending: boolean; feedback: string };

export function createBrowserPlaybackSession(
	owner: AccountSession, binding: string, show: (state: PrivateListeningState) => void, fetchFn?: typeof fetch
) {
	const lifetime = new AbortController();
	const signal = AbortSignal.any([owner.signal, lifetime.signal]);
	const isCurrent = () => !signal.aborted && owner.isCurrent();
	const api = new ApiClient({ baseUrl: '', fetchFn, session: { owner, binding } });
	const grant = createBrowserPlaybackGrant(owner, binding, async ({ trackId, playId, playbackSessionId }, signal) => {
		const { status, data } = await api.writeBrowserPlayback({ track_id: trackId, session_id: playbackSessionId, play_id: playId }, signal);
		return status === 200 ? data : undefined;
	});
	let enabled = false, uncertain = false;
	let busy: 'toggle' | 'play' | undefined;
	let expiry: ReturnType<typeof setTimeout> | undefined;
	return {
		owner, binding, isCurrent,
		dispose() { lifetime.abort(); grant.dispose(); clearTimeout(expiry); },
		async setPrivate(next: boolean) {
			if (!isCurrent() || busy) return;
			busy = 'toggle';
			show({ enabled, pending: true, feedback: '' });
			let feedback = 'Could not change private listening.';
			try {
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
				const expiryMs = typeof ack?.private_until === 'number' && Number.isSafeInteger(ack.private_until)
					? ack.private_until * 1000 - Date.now() : 0;
				uncertain = true;
				if (status !== 200 || ack?.session_id !== grant.playbackSessionId ||
					(next ? expiryMs <= 0 || expiryMs > 2_147_483_647 : ack.private_until !== null)) return;
				await owner.run(() => {
					if (!isCurrent()) return;
					clearTimeout(expiry);
					enabled = next;
					uncertain = false;
					feedback = '';
					if (next) expiry = setTimeout(() => {
						enabled = false;
						if (isCurrent()) show({ enabled, pending: busy === 'toggle', feedback: '' });
					}, expiryMs);
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
				return await grant.open({ trackId }, signal, current, publish);
			} finally { busy = undefined; }
		}
	};
}
