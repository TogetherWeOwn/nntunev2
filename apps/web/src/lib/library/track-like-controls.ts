import { SvelteSet } from 'svelte/reactivity';
import { setTrackLike } from './likes.js';
import type { LibraryRefreshUpdate } from './refresh.js';

export interface TrackLikeOwner {
	client: Parameters<typeof setTrackLike>[0];
	/** The host supplies the server-verified binding for this authorization epoch. */
	readonly binding: string;
	/** Latest listing publication for this owner only. Return the published object, not a copy. */
	readiness: () => LibraryRefreshUpdate | undefined;
	/** Undefined until the track and its confirmed Like state are known in this scope. */
	liked: (id: string) => boolean | undefined;
	confirm: (requestId: string, confirmedId: string, liked: boolean) => void;
	failed: (id: string, error: unknown) => void;
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
	trackWrite?: (binding: string, result: Promise<unknown>) => void
): TrackLikeControls {
	const pendingByOwner = new WeakMap<TrackLikeOwner, Set<string>>();

	return (id) => {
		const owner = currentOwner();
		const receipt = owner?.readiness();
		const run = receipt?.runIfCurrent;
		if (!owner || !receipt || !run || !receipt.authoritative || receipt.authBoundary || !receipt.isCurrent()) return;
		const binding = owner.binding;
		const liked = owner.liked(id);
		if (liked === undefined) return;
		let pending = pendingByOwner.get(owner);
		if (!pending) {
			pending = new SvelteSet();
			pendingByOwner.set(owner, pending);
		}
		const inFlight = pending;
		return {
			liked,
			pending: inFlight.has(id),
			async toggle() {
				const stillCurrent = () => currentOwner() === owner && owner.readiness() === receipt &&
					receipt.isCurrent() && owner.liked(id) === liked;
				if (!stillCurrent() || inFlight.has(id)) return;
				inFlight.add(id);
				try {
					const started = await run(() => {
						if (!stillCurrent()) return;
						// Return a container, not the network promise: release the auth fence before waiting.
						const result = setTrackLike(owner.client, id, !liked, binding).then(
							(confirmedId) => ({ confirmedId }), (error: unknown) => ({ error })
						);
						trackWrite?.(binding, result);
						return { result };
					}).catch(() => undefined);
					if (!started) return;
					const result = await started.result;
					await run(() => {
						if (!stillCurrent()) return;
						if ('error' in result) owner.failed(id, result.error);
						else owner.confirm(id, result.confirmedId, !liked);
					}).catch(() => undefined);
				} finally {
					inFlight.delete(id);
				}
			}
		};
	};
}
