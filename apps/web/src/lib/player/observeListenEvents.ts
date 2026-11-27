export interface ListenEvent {
	playback_id: string;
	track_id: string;
	event: 'start' | 'complete' | 'skip';
}

export type ListenAudioLike = Pick<HTMLMediaElement,
	'paused' | 'seeking' | 'ended' | 'error' | 'addEventListener' | 'removeEventListener'>;

export interface ListenObserverOptions {
	/** Track attached to this keyed media element. */
	trackId: string;
	sender: { send(event: ListenEvent): void };
	/** Play ID from the current grant for this exact source, never the track metadata. */
	grantPlayId: () => string | undefined;
	/** Shared by every attachment in the same player lifetime. */
	startedPlaybacks: Set<string>;
	canStart?: () => boolean;
}

export interface ListenObserver {
	/** End the current listen without releasing the audio element. */
	finish(): void;
	/** Release listeners and end the current listen. */
	dispose(): void;
}

/** Reformat the canonical unpadded base64url grant ID without changing its bits. */
export function playbackIdFromGrant(id: string | undefined): string | undefined {
	if (id === undefined || !/^[A-Za-z0-9_-]{21}[AQgw]$/.test(id)) return;
	const bytes = atob(id.replace(/-/g, '+').replace(/_/g, '/'));
	const h = Array.from(bytes, (byte) => byte.charCodeAt(0).toString(16).padStart(2, '0')).join('');
	return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

/** Start once per grant on `playing`; resume keeps the active listen. */
export function observeListenEvents(
	element: ListenAudioLike,
	{ trackId, sender, grantPlayId, startedPlaybacks, canStart }: ListenObserverOptions
): ListenObserver {
	let active: string | undefined;
	let endedSeen = false;

	function finish(event: 'complete' | 'skip'): void {
		const playback_id = active;
		active = undefined;
		endedSeen = false;
		if (playback_id) sender.send({ playback_id, track_id: trackId, event });
	}

	function finishAbandoned(): void {
		let ended = endedSeen;
		try { ended = !element.seeking && (ended || element.ended === true); } catch { /* Keep the sampled value. */ }
		finish(ended ? 'complete' : 'skip');
	}

	const safe = (fn: () => void) => () => {
		try { fn(); } catch { /* Never interrupt playback. */ }
	};

	const onPlaying = safe(() => {
		// A queued event can arrive after a pause, seek, or decoder failure.
		if (element.paused !== false || element.seeking !== false || element.ended !== false || element.error !== null) return;
		endedSeen = false;
		if (active || canStart?.() === false) return;
		const id = playbackIdFromGrant(grantPlayId());
		if (!id || startedPlaybacks.has(id)) return;
		startedPlaybacks.add(id);
		active = id;
		sender.send({ playback_id: id, track_id: trackId, event: 'start' });
	});
	// Teardown can clear element.ended before our ended listener runs.
	const sampleEnded = safe(() => {
		if (!element.seeking && element.ended === true) endedSeen = true;
	});
	const onSeeking = safe(() => {
		endedSeen = false;
	});
	const onEnded = safe(() => {
		if (!element.seeking && element.ended === true) finish('complete');
	});
	const finishCurrent = safe(finishAbandoned);
	const listeners: [string, () => void][] = [
		['playing', onPlaying],
		['pause', sampleEnded],
		['timeupdate', sampleEnded],
		['seeking', onSeeking],
		['ended', onEnded],
		['emptied', finishCurrent],
		['error', safe(() => finish('skip'))]
	];
	for (const [name, listener] of listeners) element.addEventListener(name, listener);
	let disposed = false;
	return {
		finish: finishCurrent,
		dispose: safe(() => {
			if (disposed) return;
			disposed = true;
			for (const [name, listener] of listeners) element.removeEventListener(name, listener);
			finishAbandoned();
		})
	};
}
