import { validSessionBinding, type AccountSession } from '../auth-fence.js';
import type { QueuedPlaySource } from './queueController.js';

type Target = { readonly trackId: string; readonly playId?: string };
type Owner = { readonly sessionBinding: string; readonly playbackSessionId: string };
type Reply = Owner & { trackId: string; playId: string; durationMs: number };
const REPLY_KEYS = ['sessionBinding', 'playbackSessionId', 'trackId', 'playId', 'durationMs'] as const;
type Prepare = (request: Owner & Target, signal: AbortSignal) => Promise<{ data: unknown; expired?: () => boolean } | undefined>;
export type BrowserPlaybackSource = QueuedPlaySource & {
	readonly trackId: string;
	readonly playId: string;
	readonly expired?: () => boolean;
};

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
			publish: (source: BrowserPlaybackSource, durationMs: number) => void, onDiscarded?: () => void
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
				const prepared = await dispatched.pending;
				const reply = prepared?.data as Reply | undefined;
				if (!reply || typeof reply !== 'object' || Object.keys(reply).length !== REPLY_KEYS.length ||
					!REPLY_KEYS.every((key) => Object.hasOwn(reply, key)) || reply.sessionBinding !== sessionBinding ||
					reply.playbackSessionId !== playbackSessionId || reply.trackId !== trackId || !validId(reply.playId) ||
					(playId !== undefined && reply.playId !== playId) || !Number.isSafeInteger(reply.durationMs) || reply.durationMs <= 0) return false;
				const { playId: issuedPlayId, durationMs } = reply;
				return (await owner.run(() => {
					if (!ownsAccount()) return false;
					if (!isCurrent()) {
						// The verified new receipt may already have replaced the previous lease.
						onDiscarded?.();
						return false;
					}
					if (prepared?.expired?.()) {
						onDiscarded?.();
						return false;
					}
					publish({ streamUrl: `/v1/plays/media/${trackId}/${playbackSessionId}/${issuedPlayId}`, trackId,
						playId: issuedPlayId, expired: prepared?.expired, isCurrent: ownsAccount }, durationMs);
					return true;
				})) ?? false;
			} catch {
				return false;
			} finally {
				opening = false;
			}
		}
	};
}
