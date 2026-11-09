import { PUBLIC_API_BASE_URL } from '$app/env/public';
import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
import { LIBRARY_AUTH_TRANSITION_KEY } from '#lib/library/cache.js';
import {
	createListenEventSender,
	observeListenEvents,
	type ListenEventSender
} from './listenEvents.js';

let sender: ListenEventSender | undefined;

function sharedSender(): ListenEventSender {
	if (!sender) {
		const created = createListenEventSender({ baseUrl: PUBLIC_API_BASE_URL });
		sender = created;
		// Pending events never cross an account boundary (same-tab and cross-tab).
		subscribeLocalAuthTransition(() => created.reset());
		if (typeof window !== 'undefined') {
			window.addEventListener('storage', (event) => {
				if (event.key === LIBRARY_AUTH_TRANSITION_KEY) created.reset();
			});
		}
	}
	return sender;
}

/**
 * Svelte action for an `<audio>`: `use:listenEvents={() => trackId}`.
 * Telemetry is best effort and must never affect playback, so every failure
 * here is contained. At an auth boundary the active playback is forgotten
 * without a terminal event, so an old session's end never uploads as the
 * new owner's.
 */
export function listenEvents(node: HTMLAudioElement, trackId: () => string | undefined) {
	let dispose = () => {};
	let unsubscribe = () => {};
	let onStorage: ((event: StorageEvent) => void) | undefined;
	try {
		const observer = observeListenEvents(node, { trackId, sender: sharedSender() });
		dispose = observer.dispose;
		unsubscribe = subscribeLocalAuthTransition(() => observer.reset());
		onStorage = (event) => {
			if (event.key === LIBRARY_AUTH_TRANSITION_KEY) observer.reset();
		};
		window.addEventListener('storage', onStorage);
	} catch {
		// Telemetry is optional.
	}
	return {
		destroy() {
			try {
				unsubscribe();
				if (onStorage) window.removeEventListener('storage', onStorage);
				dispose();
			} catch {
				// Never interfere with element teardown.
			}
		}
	};
}
