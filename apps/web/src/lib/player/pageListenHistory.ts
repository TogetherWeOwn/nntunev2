import { PUBLIC_API_BASE_URL, PUBLIC_LISTEN_HISTORY_ENABLED } from '$app/env/public';
import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
import { LIBRARY_AUTH_TRANSITION_KEY } from '#lib/library/cache.js';
import { openListenContext } from './listenContext.js';
import { createListenEventSender, type ListenEventSender } from './listenEventSender.js';
import { observeListenEvents } from './observeListenEvents.js';

/** One context per mounted player page, independent of its keyed audio elements. */
export function createPageListenHistory() {
	let sender: ListenEventSender | undefined;
	const observers = new Set<() => void>();

	return {
		mount(): () => void {
			if (!PUBLIC_LISTEN_HISTORY_ENABLED) return () => {};
			const context = openListenContext(PUBLIC_API_BASE_URL);
			const scopedSender = createListenEventSender({ baseUrl: PUBLIC_API_BASE_URL, context });
			sender = scopedSender;
			const drop = () => {
				context.drop();
				scopedSender.dispose();
				if (sender === scopedSender) sender = undefined;
			};
			const unsubscribe = subscribeLocalAuthTransition(drop);
			const onStorage = (event: StorageEvent) => {
				if (event.key === LIBRARY_AUTH_TRANSITION_KEY) drop();
			};
			window.addEventListener('storage', onStorage);
			return () => {
				for (const dispose of observers) dispose();
				context.drop();
				if (sender === scopedSender) sender = undefined;
				// Navigation may send the terminal. An auth reset still aborts that drain.
				void scopedSender.close().finally(() => {
					unsubscribe();
					window.removeEventListener('storage', onStorage);
				});
			};
		},
		attach(audio: HTMLAudioElement, trackId: string | undefined): () => void {
			if (trackId) {
				try {
					const observer = observeListenEvents(audio, {
						trackId,
						sender: { send: (event) => sender?.send(event) }
					});
					const dispose = () => {
						observers.delete(dispose);
						observer.dispose();
					};
					observers.add(dispose);
					return dispose;
				} catch {
					// Observation must not prevent the element from attaching.
				}
			}
			return () => {};
		}
	};
}
