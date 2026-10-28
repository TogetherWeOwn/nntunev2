/**
 * Audio-element orchestration for the queue page, kept free of Svelte and
 * the DOM so the ordering rules are unit-testable (the Playwright specs
 * cannot run on every host, vitest can).
 */

/** The slice of `HTMLAudioElement` the controller drives. */
export interface AudioLike {
	play(): Promise<void> | void;
	pause(): void;
	load(): void;
	getAttribute(name: string): string | null;
	readonly paused?: boolean;
	addEventListener?: EventTarget['addEventListener'];
	removeEventListener?: EventTarget['removeEventListener'];
}

export interface PlaybackDeps {
	audio: () => AudioLike | null | undefined;
	/** Resolves once the DOM reflects the latest state (Svelte's `tick`). */
	flush: () => Promise<void>;
	/** Only the current attempt can report an outcome; never passes raw errors. */
	onPlaybackFailure?: () => void;
	onPlaybackStarted?: () => void;
	/** Interruption transitions only; initial loading is not buffering. */
	onBufferingChange?: (buffering: boolean) => void;
}

export interface Playback {
	/** Observe native controls too; release subscriptions with the element. */
	observe(element: AudioLike): () => void;
	/** Start the current source, after the pending `src` update has landed. */
	play(): Promise<void>;
	pause(): void;
	/** Stop and release the loaded resource (queue cleared, `src` removed). */
	unload(): Promise<void>;
}

export function createPlayback({
	audio,
	flush,
	onPlaybackFailure,
	onPlaybackStarted,
	onBufferingChange
}: PlaybackDeps): Playback {
	// The latest intent wins: every operation bumps the transport
	// generation, and an async continuation that wakes up under a newer
	// generation is inert. Without this, a play awaiting flush would call
	// audio.play() after a later pause once its (older) flush resolves.
	// Source intent is tracked separately: only play() and unload() change
	// which source the element should hold, so unload() guards on that
	// counter alone. A pause() after unload() is redundant, not a newer
	// source intent, and must not cancel the pending load() release.
	let latestIntent = 0;
	let latestSourceIntent = 0;
	let intended = false;
	let hasPlayed = false;
	let buffering = false;
	let observed: AudioLike | undefined;
	let release = () => {};

	function setBuffering(value: boolean): void {
		if (buffering === value) return;
		buffering = value;
		onBufferingChange?.(value);
	}

	function reset(): void {
		intended = false;
		hasPlayed = false;
		setBuffering(false);
	}

	function observe(element: AudioLike): () => void {
		if (observed === element) return release;
		release();
		reset();
		observed = element;
		const source = element.getAttribute('src');
		const isCurrent = () => release === cleanup && observed === element && element === audio() &&
			element.getAttribute('src') === source;
		const canPlay = () => isCurrent() && element.paused !== true;
		const onPlay = () => {
			if (canPlay()) intended = true;
		};
		const onPlaying = () => {
			if (!intended || !canPlay()) return;
			hasPlayed = true;
			setBuffering(false);
		};
		const onWaiting = () => {
			if (intended && hasPlayed && canPlay()) setBuffering(true);
		};
		const onStop = () => {
			// The page's ended handler may already have started a new attempt
			// awaiting flush. That reset intent must not be cancelled by this end.
			if (!isCurrent() || !intended) return;
			++latestIntent;
			reset();
		};
		// A stalled download can still have audible buffered data. Only waiting
		// after a playing event signals interruption; no timer or recovery action.
		const listeners: [string, EventListener][] = [
			['play', onPlay], ['playing', onPlaying], ['waiting', onWaiting],
			['pause', onStop], ['ended', onStop], ['error', onStop]
		];
		if (element.addEventListener && element.removeEventListener) {
			for (const [name, listener] of listeners) element.addEventListener(name, listener);
		}
		const cleanup = () => {
			for (const [name, listener] of listeners) element.removeEventListener?.(name, listener);
			if (release !== cleanup || observed !== element) return;
			observed = undefined;
			reset();
		};
		release = cleanup;
		return cleanup;
	}

	return {
		observe,
		async play() {
			const intent = ++latestIntent;
			++latestSourceIntent;
			reset();
			// play() before the flush targets the old source, and the src swap
			// that follows aborts it: the new track would load silent.
			await flush();
			if (intent !== latestIntent) return;
			const element = audio();
			if (!element) return;
			observe(element);
			intended = true;
			try {
				await element.play();
			} catch {
				// Navigation still resolves, but only the current attempt may
				// expose a failure. A pause/source change often rejects old plays.
				if (intent === latestIntent && element === audio()) {
					reset();
					onPlaybackFailure?.();
				}
				return;
			}
			if (intent === latestIntent && element === audio()) onPlaybackStarted?.();
		},
		pause() {
			++latestIntent;
			reset();
			audio()?.pause();
		},
		async unload() {
			++latestIntent;
			const sourceIntent = ++latestSourceIntent;
			release();
			audio()?.pause();
			// Removing `src` does not stop a loaded element; load() on the
			// emptied element releases it. Needs the flush so `src` is gone first.
			await flush();
			if (sourceIntent !== latestSourceIntent) return;
			audio()?.load();
		}
	};
}

/**
 * True when an `ended` event belongs to the source the queue currently
 * points at. A queued `ended` from a superseded track (the queue already
 * moved on, `src` not yet swapped) must not advance the new track.
 */
export function endedForCurrent(audio: AudioLike, currentStreamUrl: string | undefined): boolean {
	return currentStreamUrl !== undefined && audio.getAttribute('src') === currentStreamUrl;
}
