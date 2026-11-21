import { SvelteSet } from 'svelte/reactivity';
import { setTrackLike } from './likes.js';

export interface TrackLikeOwner {
	/** The host must bind requests to this ownership scope, not just ambient cookies. */
	client: Parameters<typeof setTrackLike>[0];
	/** Undefined until the track and its confirmed Like state are known in this scope. */
	liked: (id: string) => boolean | undefined;
	confirm: (requestId: string, confirmedId: string, liked: boolean) => void;
	failed: (id: string) => void;
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
	currentOwner: () => TrackLikeOwner | undefined
): TrackLikeControls {
	const pendingByOwner = new WeakMap<TrackLikeOwner, Set<string>>();

	return (id) => {
		const owner = currentOwner();
		const liked = owner?.liked(id);
		if (!owner || liked === undefined) return;
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
				// A rendered control can outlive the state or owner that provided it.
				const stillCurrent = () => currentOwner() === owner && owner.liked(id) === liked;
				if (!stillCurrent() || inFlight.has(id)) return;
				inFlight.add(id);
				try {
					let confirmedId: string;
					try {
						confirmedId = await setTrackLike(owner.client, id, !liked);
					} catch {
						if (stillCurrent()) owner.failed(id);
						return;
					}
					if (stillCurrent()) owner.confirm(id, confirmedId, !liked);
				} finally {
					inFlight.delete(id);
				}
			}
		};
	};
}
