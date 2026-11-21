import type { Readable } from 'svelte/store';
import { displayArtist, displayTitle } from '../library/order.js';
import { applyMediaSession, clearMediaSession, getMediaSession } from './mediaSession.js';
import type { QueueState, Track } from './queue.js';

/** The native attachment owns this registration, not a route or queue revision. */
export function bindSystemTransport(
	queue: Readable<QueueState>,
	readTrack: () => Track | null,
	{ playCurrent, pauseCurrent }: { playCurrent: () => Promise<void>; pauseCurrent: () => void },
	signal: AbortSignal
): void {
	const session = getMediaSession();
	if (!session || signal.aborted) return;
	const options = { session };
	const guard = (action: () => void) => () => {
		// Unregistering cannot recall a callback already queued by the OS.
		if (!signal.aborted && readTrack()) action();
	};
	const handlers = { onPlay: guard(() => void playCurrent()), onPause: guard(pauseCurrent) };
	let published: Track | null = null;
	let unsubscribe: (() => void) | undefined;
	signal.addEventListener('abort', () => {
		unsubscribe?.();
		if (published) clearMediaSession(options);
	}, { once: true });
	unsubscribe = queue.subscribe(() => {
		if (signal.aborted) return;
		const track = readTrack();
		if (!track || track === published) return;
		published = track;
		// Match the compact identity; don't introduce OS requests for external cover art.
		applyMediaSession({ ...track, title: displayTitle(track), artist: displayArtist(track) ?? '', artworkUrl: null },
			handlers, options);
	});
	if (signal.aborted) unsubscribe();
}
