import { currentTrack } from '../../src/lib/player/queue.js';
import { displayTitle } from '../../src/lib/library/order.js';
import type { PlayerSession } from '../../src/lib/player/session.js';

export type FavoritesQueueSnapshot = {
	ids: string[];
	currentIndex: number;
	currentId: string | null;
	selectedTitle: string | null;
};

declare global {
	interface Window {
		__favoritesQueueOwner?: { readonly snapshot: FavoritesQueueSnapshot; readonly selectionVersion?: number };
	}
}

// Imported only by the dedicated test server's layout transform. No session is
// created here: this subscription observes the real layout's existing owner.
export function attachFavoritesObserver(player: PlayerSession): () => void {
	if (window.__favoritesQueueOwner) throw new Error('Duplicate Favorites queue observer');
	let latest: FavoritesQueueSnapshot | undefined;
	let disposed = false;
	let selectionVersion = 0;
	const unsubscribe = player.subscribe((state) => {
		const selected = currentTrack(state.queue);
		const currentId = selected?.id ?? null;
		const selectedTitle = selected ? displayTitle(selected) : null;
		if (latest && (latest.currentIndex !== state.queue.currentIndex ||
			latest.currentId !== currentId || latest.selectedTitle !== selectedTitle)) {
			selectionVersion += 1;
		}
		latest = {
			ids: state.queue.tracks.map((track) => track.id),
			currentIndex: state.queue.currentIndex,
			currentId,
			selectedTitle
		};
	});
	const binding = Object.freeze({
		get snapshot(): FavoritesQueueSnapshot {
			if (disposed || !latest) throw new Error('Disposed or uninitialized Favorites queue observer');
			return structuredClone(latest);
		},
		get selectionVersion(): number {
			if (disposed || !latest) throw new Error('Disposed or uninitialized Favorites queue observer');
			return selectionVersion;
		}
	});
	window.__favoritesQueueOwner = binding;
	return () => {
		disposed = true;
		unsubscribe();
		if (window.__favoritesQueueOwner !== binding) throw new Error('Replaced Favorites queue owner');
		delete window.__favoritesQueueOwner;
	};
}
