/**
 * Right-panel slot (roadmap W10).
 *
 * The app shell renders `<aside data-testid="right-panel">` only while a
 * snippet is set; routes publish panel content here (nothing does yet —
 * W10 ships the mechanism unfilled). Same listener idiom as the other
 * shell stores: no storage, no network, no DOM access.
 */

import type { Snippet } from 'svelte';

type RightPanelListener = (panel: Snippet | null) => void;

let current: Snippet | null = null;
const listeners = new Set<RightPanelListener>();

/**
 * Subscribe to the slot (app shell). The listener fires immediately with
 * the current value. Returns the unsubscribe.
 */
export function onRightPanel(listener: RightPanelListener): () => void {
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

/** Read the current slot without subscribing (tests, non-rendered use). */
export function readRightPanel(): Snippet | null {
	return current;
}

/** Publish panel content, or null to close the slot. */
export function setRightPanel(panel: Snippet | null): void {
	current = panel;
	for (const listener of listeners) {
		try {
			listener(current);
		} catch {
			// Display is best-effort; a throwing listener must not break the
			// store or starve the remaining subscribers.
		}
	}
}
