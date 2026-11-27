import { SvelteSet } from 'svelte/reactivity';
import { setTrackLike, trackLikeTarget } from './likes.js';
import type { LibraryRefreshUpdate } from './refresh.js';
import { validBinding } from './session-verification.js';
import { saveTrackTaste, type TasteSignal, type TrackTaste } from './taste.js';

export interface TrackLikeOwner {
	client: Parameters<typeof setTrackLike>[0];
	/** The host supplies the server-verified binding for this authorization epoch. */
	readonly binding: string;
	/** Verified Like readiness for this owner. Return the same object until it is retired. */
	readiness: () => LibraryRefreshUpdate | undefined;
	/** Undefined until the track and its confirmed Like state are known in this scope. */
	liked: (id: string) => boolean | undefined;
	confirm: (requestId: string, confirmedId: string, liked: boolean) => void;
	failed: (id: string, error: unknown, taste?: boolean) => void;
	taste?: {
		/** Preferences can require a newer listing than the Like button. */
		isCurrent?: () => boolean;
		read: (id: string) => TrackTaste | null | undefined;
		confirm: (id: string, value: TrackTaste | null) => void;
		verify: () => Promise<boolean>;
		invalidate: () => void;
	};
}

export interface TrackLikeControl {
	liked: boolean;
	pending: boolean;
	toggle: () => Promise<void>;
}

export type TrackLikeControls = (id: string) => TrackLikeControl | undefined;

/** Share one instance across track and queue rows. An owner is unique to one
 * authorization epoch; currentOwner must stop returning it upon invalidation. */
export function createTrackLikeControls(
	currentOwner: () => TrackLikeOwner | undefined,
	trackWrite?: (binding: string, settled: Promise<void>) => void
) {
	const pendingByOwner = new WeakMap<TrackLikeOwner, Set<string>>();

	return (id: string) => {
		// The host supplies the resolved target. Preserve exact mixed-case job IDs.
		if (!trackLikeTarget(id, id)) return;
		const owner = currentOwner();
		const receipt = owner?.readiness();
		const run = receipt?.runIfCurrent;
		if (!owner || !receipt || !run) return;
		const binding = owner.binding;
		const ready = () => receipt.authoritative === true && receipt.authBoundary === false && receipt.isCurrent() &&
			(receipt.sessionBinding === undefined || receipt.sessionBinding === binding);
		if (!validBinding(binding) || !ready()) return;
		const liked = owner.liked(id);
		if (typeof liked !== 'boolean') return;
		let pending = pendingByOwner.get(owner);
		if (!pending) {
			pending = new SvelteSet();
			pendingByOwner.set(owner, pending);
		}
		const inFlight = pending;
		const taste = owner.taste;
		const saved = taste?.read(id);
		const visible = saved?.signal === 'love' || (saved?.until != null && saved.until <= Date.now() / 1000) ? null : saved;
		const stillCurrent = () => currentOwner() === owner && owner.readiness() === receipt &&
			owner.binding === binding && owner.taste === taste && ready() && owner.liked(id) === liked && taste?.read(id) === saved;
		const write = async <T,>(start: () => Promise<T>, confirm: (value: T) => void, preference?: typeof taste) => {
			const canWrite = () => stillCurrent() && (!preference?.isCurrent || preference.isCurrent() === true);
			if (!canWrite() || inFlight.has(id)) return;
			inFlight.add(id);
			let finish!: () => void;
			const settled = new Promise<void>((resolve) => { finish = resolve; });
			try {
				const started = await run(() => {
					if (!canWrite()) return;
					// Box network work so it never holds the auth fence. Refresh tracks
					// the write through confirmation, including any later fence checks.
					const result = start().then((value) => ({ value }), (error: unknown) => ({ error }));
					trackWrite?.(binding, settled);
					return { result };
				});
				if (!started) return;
				const result = await started.result;
				let verified = true;
				if (!('error' in result) && preference) {
					const proof = await run(() => canWrite() ? { result: preference.verify().catch(() => false) } : undefined);
					if (!proof) return;
					verified = await proof.result === true;
				}
				await run(() => {
					if (!canWrite()) return;
					if ('error' in result) {
						if (preference) owner.failed(id, result.error, true);
						else owner.failed(id, result.error);
					} else if (!verified) preference?.invalidate();
					else confirm(result.value);
				});
			} catch {
				// A lost auth fence is not a reason to replay or report against a new scope.
			} finally {
				inFlight.delete(id);
				finish();
			}
		};
		return {
			liked,
			pending: inFlight.has(id),
			toggle: () => write(() => setTrackLike(owner.client, id, !liked),
				(confirmedId) => owner.confirm(id, confirmedId, !liked)),
			taste: taste && saved !== undefined ? {
				saved: visible?.signal,
				set: async (signal: TasteSignal | null) => {
					if (signal === null && !visible) return;
					if (signal !== null && signal !== 'dislike' && signal !== 'hide' && signal !== 'ban' && signal !== 'snooze') return;
					await write(() => saveTrackTaste(owner.client, binding, id, signal), (value) => taste.confirm(id, value), taste);
				}
			} : undefined
		};
	};
}
