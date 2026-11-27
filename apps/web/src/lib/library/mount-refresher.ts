import { subscribeLocalAuthTransition } from '../auth-transition.js';
import type { LibraryRefresher } from './refresh.js';

/** Start a mounted route's refresh and listen for delivered auth transitions. */
export function mountLibraryRefresher(refresher: LibraryRefresher): () => void {
	let mounted = true;
	window.addEventListener('storage', refresher.handleAuthTransition);
	const unsubscribe = subscribeLocalAuthTransition(refresher.handleLocalAuthTransition);
	void refresher.refresh().catch(() => { if (mounted) refresher.suspendVerification(); });

	return () => {
		if (!mounted) return;
		mounted = false;
		window.removeEventListener('storage', refresher.handleAuthTransition);
		unsubscribe();
		refresher.dispose();
	};
}
