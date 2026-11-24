import { validSessionBinding, type AccountSession } from '../auth-fence.js';
import type { QueuedPlaySource } from './queueController.js';

type Target = { readonly trackId: string; readonly playId?: string };
type Owner = { readonly sessionBinding: string; readonly playbackSessionId: string };
type Reply = Owner & { trackId: string; playId: string; durationMs: number };
type Prepare = (request: Owner & Target, signal: AbortSignal) => Promise<unknown>;

function validId(value: unknown): value is string {
	return typeof value === 'string' && /^[A-Za-z0-9_-]{1,512}$/.test(value);
}

/** Expose only the media locator and duration for one verified account and tab. */
export function createBrowserPlaybackGrant(owner: AccountSession, sessionBinding: string, prepare?: Prepare) {
	const lifetime = new AbortController();
	const playbackSessionId = crypto.randomUUID();
	let opening = false;
	return {
		playbackSessionId,
		dispose: () => lifetime.abort(),
		async open(
			{ trackId, playId }: Target, cancellation: AbortSignal, isCurrent: () => boolean,
			publish: (source: QueuedPlaySource, durationMs: number) => void
		): Promise<boolean> {
			const signal = AbortSignal.any([owner.signal, lifetime.signal, cancellation]);
			const ownsAccount = () => !signal.aborted && owner.isCurrent();
			const current = () => ownsAccount() && isCurrent();
			if (opening || !prepare || !current() || !validSessionBinding(sessionBinding) || !validId(trackId) ||
				(playId !== undefined && !validId(playId))) return false;
			opening = true;
			try {
				// Hold the auth fence for dispatch and publication. The transport
				// observes cancellation while its response is pending.
				const dispatched = await owner.run(() => current() && {
					pending: prepare({ sessionBinding, playbackSessionId, trackId, playId }, signal).catch(() => undefined)
				});
				if (!dispatched) return false;
				const reply = await dispatched.pending as Reply | undefined;
				if (!reply || Object.keys(reply).length !== 5 || reply.sessionBinding !== sessionBinding ||
					reply.playbackSessionId !== playbackSessionId || reply.trackId !== trackId || !validId(reply.playId) ||
					(playId !== undefined && reply.playId !== playId) || !Number.isSafeInteger(reply.durationMs) || reply.durationMs <= 0) return false;
				const { playId: issuedPlayId, durationMs } = reply;
				return (await owner.run(() => {
					if (!current()) return false;
					publish({ streamUrl: `/v1/plays/media/${trackId}/${playbackSessionId}/${issuedPlayId}`, isCurrent: ownsAccount }, durationMs);
					return true;
				})) ?? false;
			} finally {
				opening = false;
			}
		}
	};
}
