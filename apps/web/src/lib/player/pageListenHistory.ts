import { PUBLIC_API_BASE_URL } from '$app/env/public';
import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
import { LIBRARY_AUTH_TRANSITION_KEY } from '#lib/library/cache.js';
import { openListenContext } from './listenContext.js';
import { createListenEventSender, type ListenEventSender } from './listenEventSender.js';
import { observeListenEvents, type ListenObserver } from './observeListenEvents.js';

/** One context per mounted player page, independent of its keyed audio elements. */
export function createPageListenHistory() {
	let sender: ListenEventSender | undefined;
	let pageHidden = false;
	const observers = new Map<HTMLAudioElement, ListenObserver>();

	return {
		mount(): () => void {
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
			const onPageHide = (event: PageTransitionEvent) => {
				pageHidden = true;
				if (event.persisted) {
					// End the listen, but keep the observers and original auth scope for restoration.
					for (const observer of observers.values()) observer.finish();
				} else {
					stop();
				}
			};
			const onPageShow = (event: PageTransitionEvent) => {
				if (event.persisted) pageHidden = false;
			};
			window.addEventListener('storage', onStorage);
			window.addEventListener('pagehide', onPageHide);
			window.addEventListener('pageshow', onPageShow);
			let stopped = false;
			function stop() {
				if (stopped) return;
				stopped = true;
				window.removeEventListener('pagehide', onPageHide);
				window.removeEventListener('pageshow', onPageShow);
				for (const observer of observers.values()) observer.dispose();
				observers.clear();
				context.drop();
				if (sender === scopedSender) sender = undefined;
				// Navigation may send the terminal. An auth reset still aborts that drain.
				void scopedSender.close().finally(() => {
					unsubscribe();
					window.removeEventListener('storage', onStorage);
				});
			}
			return stop;
		},
		finish(audio: HTMLAudioElement | undefined): void {
			if (audio) observers.get(audio)?.finish();
		},
		attach(
			audio: HTMLAudioElement,
			trackId: string | undefined,
			grantPlayId: () => string | undefined
		): () => void {
			if (trackId) {
				try {
					const observer = observeListenEvents(audio, {
						trackId,
						grantPlayId,
						canStart: () => !pageHidden,
						sender: { send: (event) => sender?.send(event) }
					});
					observers.set(audio, observer);
					return () => {
						observers.delete(audio);
						observer.dispose();
					};
				} catch {
					// Observation must not prevent the element from attaching.
				}
			}
			return () => {};
		}
	};
}
