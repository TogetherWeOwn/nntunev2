import type { ListenEvent, ListenEventSender } from './listenEventSender.js';

export interface ListenAudioLike {
	readonly ended?: boolean;
	addEventListener(name: string, listener: () => void): void;
	removeEventListener(name: string, listener: () => void): void;
}

export interface ListenObserverOptions {
	/** Track attached to this keyed media element. */
	trackId: string;
	sender: Pick<ListenEventSender, 'send'>;
	newId?: () => string;
}

export interface ListenObserver {
	/** Release listeners; a started playback ends (complete if the media ended, else skip). */
	dispose(): void;
}

/** RFC 4122 v4 id; `randomUUID` is absent in insecure contexts. */
export function newPlaybackId(): string {
	if (typeof crypto.randomUUID === 'function') return crypto.randomUUID();
	const b = crypto.getRandomValues(new Uint8Array(16));
	b[6] = (b[6] & 0x0f) | 0x40;
	b[8] = (b[8] & 0x3f) | 0x80;
	const h = [...b].map((x) => x.toString(16).padStart(2, '0')).join('');
	return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

/**
 * Observe one audio element. `start` fires on the first actual `playing`
 * of a playback; resume after pause or buffering is the same playback.
 * `complete` fires on natural end (a repeat restart then gets a fresh
 * playback id). `skip` fires when a started playback is abandoned: source
 * emptied, element released or the observer disposed. A playback that never
 * started (failed load) emits nothing.
 *
 * Page `onended` handlers can swap the source and tear this element down
 * before our own `ended` listener runs (microtasks run between listeners).
 * So a natural end is also recognised from `element.ended`, sampled on
 * `pause`/`timeupdate` too, because teardown clears it before `emptied`.
 *
 * Every callback is contained: a failing sender or id source never reaches
 * the element's event dispatch or its teardown.
 */
export function observeListenEvents(
	element: ListenAudioLike,
	{ trackId, sender, newId = newPlaybackId }: ListenObserverOptions
): ListenObserver {
	let active: string | undefined;
	let endedSeen = false;

	function emit(playback_id: string, track_id: string, event: ListenEvent['event']): void {
		try {
			sender.send({ playback_id, track_id, event });
		} catch {
			// Best effort.
		}
	}

	function finish(event: 'complete' | 'skip'): void {
		const playback = active;
		active = undefined;
		endedSeen = false;
		if (playback) emit(playback, trackId, event);
	}

	function finishAbandoned(): void {
		let ended = endedSeen;
		try {
			ended = ended || element.ended === true;
		} catch {
			// Fall back to what was sampled earlier.
		}
		finish(ended ? 'complete' : 'skip');
	}

	function safe(fn: () => void): () => void {
		return () => {
			try {
				fn();
			} catch {
				// Never error playback.
			}
		};
	}

	const onPlaying = safe(() => {
		endedSeen = false;
		if (active) return;
		active = newId();
		emit(active, trackId, 'start');
	});
	const sampleEnded = safe(() => {
		if (element.ended === true) endedSeen = true;
	});
	const onSeeking = safe(() => {
		endedSeen = false;
	});
	const onEnded = safe(() => finish('complete'));
	const onEmptied = safe(finishAbandoned);
	const listeners: [string, () => void][] = [
		['playing', onPlaying],
		['pause', sampleEnded],
		['timeupdate', sampleEnded],
		['seeking', onSeeking],
		['ended', onEnded],
		['emptied', onEmptied]
	];
	for (const [name, listener] of listeners) element.addEventListener(name, listener);
	let disposed = false;
	return {
		dispose: safe(() => {
			if (disposed) return;
			disposed = true;
			for (const [name, listener] of listeners) element.removeEventListener(name, listener);
			finishAbandoned();
		})
	};
}
