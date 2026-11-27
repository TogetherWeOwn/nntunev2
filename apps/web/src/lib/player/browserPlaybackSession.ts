import { API_REQUEST_TIMEOUT_MS, ApiClient, ApiError } from '../api/client.js';
import { validSessionBinding, type AccountSession } from '../auth-fence.js';
import { createBrowserPlaybackGrant, type BrowserPlaybackSource } from './browserPlaybackGrant.js';

export type PrivateListeningState = { enabled: boolean; pending: boolean; feedback: string };
const PRIVATE_TTL_MS = 6 * 60 * 60 * 1000;
const MAX_GRANT_TTL_SECONDS = 900; // Matches the server's play grant cap.
export function createBrowserPlaybackSession(
	owner: AccountSession, binding: string, show: (state: PrivateListeningState) => void, fetchFn?: typeof fetch
) {
	const lifetime = new AbortController();
	const signal = AbortSignal.any([owner.signal, lifetime.signal]);
	const isCurrent = () => !signal.aborted && owner.isCurrent() && validSessionBinding(binding);
	const api = new ApiClient({ baseUrl: '', fetchFn, session: { owner, binding } });
	const grant = createBrowserPlaybackGrant(owner, binding, async ({ trackId, playbackSessionId, playId }, signal) => {
		const wall = Date.now(), monotonic = performance.now();
		const { status, data, headers } = await api.writeBrowserPlayback({ track_id: trackId, session_id: playbackSessionId, play_id: playId }, signal);
		if (status !== 200) return;
		const ttl = headers.get('x-nntune-playback-ttl');
		if (ttl === null) return { data };
		const seconds = Number(ttl);
		if (!/^[1-9]\d{0,2}$/.test(ttl) || seconds > MAX_GRANT_TTL_SECONDS) return { data, expired: () => true };
		const duration = seconds * 1000;
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
	let incumbent: Pick<BrowserPlaybackSource, 'trackId' | 'playId'> | undefined;
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
			if (!isCurrent() || pending || typeof next !== 'boolean') return;
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
					uncertain = true;
					if (status !== 200 || !ack || typeof ack !== 'object' || Object.keys(ack).length !== 2 ||
						!Object.hasOwn(ack, 'session_id') || !Object.hasOwn(ack, 'private_until') ||
						ack.session_id !== grant.playbackSessionId) return;
					const until = ack.private_until;
					const advertised = typeof until === 'number' && Number.isSafeInteger(until * 1000) &&
						Number.isInteger(until) ? until * 1000 - wall : 0;
					const remaining = () => Math.min(PRIVATE_TTL_MS, advertised) - API_REQUEST_TIMEOUT_MS -
						Math.max(Date.now() - wall, performance.now() - monotonic);
					if (next ? remaining() <= 0 : until !== null) return;
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
					uncertain ||= !(error instanceof ApiError && error.status >= 400 && error.status < 500 && error.status !== 408);
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
				const allowed = playId === undefined ? !uncertain : incumbent?.trackId === trackId && incumbent?.playId === playId;
				return allowed && current();
			}, (source) => {
				incumbent = source;
				publish(source);
			}, () => {
				if (playId === undefined) incumbent = undefined;
				onDiscarded?.();
			}));
		}
	};
}
