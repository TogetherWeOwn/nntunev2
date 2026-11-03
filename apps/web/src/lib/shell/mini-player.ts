/**
 * Mobile mini-player hint (roadmap W10).
 *
 * The library page writes the current selection here through
 * `replaceQueue`; the app shell reads it to render the mobile mini-player
 * (a title plus a link back to Your Library — the full player bar arrives
 * with W11). Metadata only: track id plus display title/artist, never
 * audio URLs or playback state. Same listener idiom as `onPaywall` and the
 * toast store: no storage, no network, no DOM access.
 */

export interface MiniPlayerHint {
	trackId: string;
	title: string;
	artist: string | null;
}

type MiniPlayerListener = (hint: MiniPlayerHint | null) => void;

let current: MiniPlayerHint | null = null;
const listeners = new Set<MiniPlayerListener>();

function emit(): void {
	for (const listener of listeners) {
		try {
			listener(current);
		} catch {
			// Display is best-effort; a throwing listener must not break the
			// store or starve the remaining subscribers.
		}
	}
}

/**
 * Subscribe to the hint (app shell). The listener fires immediately with
 * the current value. Returns the unsubscribe.
 */
export function onMiniPlayerHint(listener: MiniPlayerListener): () => void {
	listeners.add(listener);
	try {
		listener(current);
	} catch {
		// A throwing initial listener still subscribes; the error stays local.
	}
	return () => {
		listeners.delete(listener);
	};
}

/** Read the current hint without subscribing (tests, non-rendered use). */
export function readMiniPlayerHint(): MiniPlayerHint | null {
	return current;
}

/** Record a selection; replaces any previous hint. */
export function setMiniPlayerHint(hint: MiniPlayerHint): void {
	current = hint;
	emit();
}

/** Drop the hint (emptied queue, auth boundary, sign-out). */
export function clearMiniPlayerHint(): void {
	if (current === null) return;
	current = null;
	emit();
}
