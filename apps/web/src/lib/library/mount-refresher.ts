import { subscribeLocalAuthTransition } from '../auth-transition.js';
import type { LibraryRefresher } from './refresh.js';

/** Start a mounted route's refresh and listen for delivered auth transitions. */
export function mountLibraryRefresher(refresher: LibraryRefresher): () => void {
	window.addEventListener('storage', refresher.handleAuthTransition);
	const unsubscribe = subscribeLocalAuthTransition(refresher.handleLocalAuthTransition);
	void refresher.refresh();

	return () => {
		window.removeEventListener('storage', refresher.handleAuthTransition);
		unsubscribe();
		refresher.dispose();
	};
}
