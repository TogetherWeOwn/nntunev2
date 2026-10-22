/**
 * Same-window auth-transition dispatch.
 *
 * The security page calls {@link notifyLocalAuthTransition} after a
 * qualifying result (logout or revoke-all fulfilled success, or a typed
 * HTTP401) so every mounted consumer in this tab — library, provider, BYOP —
 * resets through the same callback it uses for the delivered cross-tab
 * transition (`nntune.library.auth-transition.v1` storage event). Only
 * qualifying security-action completions notify; sign-in completion and
 * every other `invalidateLibraryCache()` caller must not.
 *
 * Both entry points are no-throw, and a throwing listener can never suppress
 * later subscribers: each notification snapshots the subscriber set and
 * isolates listener failures. Delivery is best effort with no acknowledgement
 * and no all-tabs/global-wipe claim — suspended, closed or non-listening tabs
 * keep their state until their existing checks run. Receivers never
 * rebroadcast, request, navigate or retry.
 */

export type LocalAuthTransitionListener = () => void;

const listeners = new Set<LocalAuthTransitionListener>();

/**
 * Subscribe to same-window auth transitions. Returns an idempotent
 * unsubscribe function; both are safe to call during notification.
 */
export function subscribeLocalAuthTransition(
	listener: LocalAuthTransitionListener
): () => void {
	try {
		listeners.add(listener);
	} catch {
		// Subscription must never throw, even on a hostile Set.
	}
	let unsubscribed = false;
	return () => {
		if (unsubscribed) return;
		unsubscribed = true;
		try {
			listeners.delete(listener);
		} catch {
			// Unsubscribe must never throw either.
		}
	};
}

/**
 * Notify mounted same-window consumers of a qualifying auth transition.
 * A no-op without subscribers; never throws.
 */
export function notifyLocalAuthTransition(): void {
	let snapshot: LocalAuthTransitionListener[];
	try {
		snapshot = [...listeners];
	} catch {
		return;
	}
	for (const listener of snapshot) {
		try {
			listener();
		} catch {
			// One failing listener must never suppress the other local resets.
		}
	}
}
