import { subscribeLocalAuthTransition } from '../auth-transition.js';
import { LIBRARY_AUTH_TRANSITION_KEY } from '../library/cache.js';
import { observeListenEvents, type ListenEvent, type ListenObserver } from './observeListenEvents.js';

// Refuse new starts at capacity to preserve exact replay detection for the account scope.
const MAX_STARTED_PLAYBACKS = 16_384;

interface PlayerListenScope {
	send(event: ListenEvent): void;
	/** Drain ready terminals without retrying. */
	close(): Promise<void>;
	/** Invalidate this scope and abort pending work, including any drain. */
	dispose(): void;
}

/** Mount once at the shared player, not in a route child or for each keyed audio node. */
export function createPlayerListenHistory(scope: PlayerListenScope, lifecycle: Window = window) {
	const observers = new Map<HTMLAudioElement, ListenObserver>();
	const startedPlaybacks = new Set<string>();
	const listeners = new AbortController();
	const { signal } = listeners;
	let phase: 'observing' | 'draining' | 'closed' = 'observing';
	let hidden = false;

	function clear() {
		for (const observer of observers.values()) observer.dispose();
		observers.clear();
	}
	function stopListening() {
		phase = 'closed';
		startedPlaybacks.clear();
		unsubscribe();
		listeners.abort();
	}
	function resetAtAuthBoundary() {
		if (phase === 'closed') return;
		stopListening();
		try { scope.dispose(); } finally { clear(); }
	}
	const unsubscribe = subscribeLocalAuthTransition(resetAtAuthBoundary);

	async function drain() {
		try { await scope.close(); } catch { /* History is best effort. */ }
		finally { stopListening(); }
	}
	function dispose() {
		if (phase !== 'observing') return;
		phase = 'draining';
		clear();
		// Keep auth signals subscribed until the terminal drain settles.
		if (!signal.aborted) void drain();
	}
	lifecycle.addEventListener('storage', ({ key }) => {
		if (key === LIBRARY_AUTH_TRANSITION_KEY) resetAtAuthBoundary();
	}, { signal });
	lifecycle.addEventListener('pagehide', ({ persisted }) => {
		hidden = true;
		if (persisted) for (const observer of observers.values()) observer.finish();
		else dispose();
	}, { signal });
	lifecycle.addEventListener('pageshow', ({ persisted }) => {
		if (persisted) hidden = false;
	}, { signal });

	return {
		dispose,
		resetAtAuthBoundary,
		finish: (audio: HTMLAudioElement) => observers.get(audio)?.finish(),
		attach(audio: HTMLAudioElement, trackId?: string, grantPlayId?: () => string | undefined) {
			if (phase !== 'observing' || typeof grantPlayId !== 'function' || observers.has(audio) ||
				typeof trackId !== 'string' || trackId.length > 128 || !trackId.trim() ||
				/\p{Cc}/u.test(trackId) || new TextEncoder().encode(trackId).length > 128) return () => {};
			const observer = observeListenEvents(audio, {
				trackId,
				grantPlayId,
				startedPlaybacks,
				canStart: () => phase === 'observing' && !hidden && startedPlaybacks.size < MAX_STARTED_PLAYBACKS,
				sender: { send: (event) => { if (phase !== 'closed') scope.send(event); } }
			});
			observers.set(audio, observer);
			return () => {
				if (observers.get(audio) === observer) observers.delete(audio);
				observer.dispose();
			};
		}
	};
}
