import { API_REQUEST_TIMEOUT_MS, ApiClient, ApiError } from '../api/client.js';
import type { AccountSession } from '../auth-fence.js';
import { createBrowserPlaybackGrant, type BrowserPlaybackSource } from './browserPlaybackGrant.js';

export type PrivateListeningState = { enabled: boolean; pending: boolean; feedback: string };
const PRIVATE_TTL_MS = 6 * 60 * 60 * 1000;
export function createBrowserPlaybackSession(
	owner: AccountSession, binding: string, show: (state: PrivateListeningState) => void, fetchFn?: typeof fetch
) {
	const lifetime = new AbortController();
	const signal = AbortSignal.any([owner.signal, lifetime.signal]);
	const isCurrent = () => !signal.aborted && owner.isCurrent();
	const api = new ApiClient({ baseUrl: '', fetchFn, session: { owner, binding } });
	const grant = createBrowserPlaybackGrant(owner, binding, async ({ trackId, playbackSessionId, playId }, signal) => {
		const wall = Date.now(), monotonic = performance.now();
		const { status, data, headers } = await api.writeBrowserPlayback({ track_id: trackId, session_id: playbackSessionId, play_id: playId }, signal);
		if (status !== 200) return;
		const ttl = headers.get('x-nntune-playback-ttl');
		if (ttl === null || !/^\d{1,3}$/.test(ttl)) return { data };
		const duration = Number(ttl) * 1000;
		return { data, expired: () => Math.max(Date.now() - wall, performance.now() - monotonic) >= duration };
	});
	let enabled = false, uncertain = false, pending = false;
	let tail = Promise.resolve();
	function serialize<T>(work: () => Promise<T>) {
		const result = tail.then(work);
		tail = result.then(() => {}, () => {});
		return result;
	}
	let expiry: ReturnType<typeof setTimeout> | undefined, expired: (() => boolean) | undefined;
	function expire() {
		if (!enabled || !isCurrent()) return;
		enabled = false;
		uncertain = true;
		show({ enabled, pending, feedback: 'Confirm private listening again to start another track.' });
	}
	return {
		owner, binding,
		dispose() { lifetime.abort(); grant.dispose(); clearTimeout(expiry); },
		setPrivate(next: boolean) {
			if (!isCurrent() || pending) return;
			pending = true;
			show({ enabled, pending, feedback: '' });
			return serialize(async () => {
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
					if ('error' in result) throw result.error;
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
				} catch (error) {
					uncertain ||= !(error instanceof ApiError && error.status >= 400 && error.status < 500);
				} finally {
					pending = false;
					if (isCurrent()) show({ enabled, pending, feedback });
				}
			});
		},
		open(trackId: string, current: () => boolean, publish: (source: BrowserPlaybackSource) => void, playId?: string,
			onDiscarded?: () => void) {
			return serialize(() => grant.open({ trackId, playId }, signal, () => {
				if (expired?.()) expire();
				return (playId !== undefined || !uncertain) && current();
			}, publish, onDiscarded));
		}
	};
}
