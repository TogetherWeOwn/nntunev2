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
}

export interface PlaybackDeps {
	audio: () => AudioLike | null | undefined;
	/** Resolves once the DOM reflects the latest state (Svelte's `tick`). */
	flush: () => Promise<void>;
}

export interface Playback {
	/** Start the current source, after the pending `src` update has landed. */
	play(): Promise<void>;
	pause(): void;
	/** Stop and release the loaded resource (queue cleared, `src` removed). */
	unload(): Promise<void>;
}

export function createPlayback({ audio, flush }: PlaybackDeps): Playback {
	// The latest intent wins: every operation bumps the generation, and an
	// async continuation that wakes up under a newer generation is inert.
	// Without this, a play awaiting flush would call audio.play() after a
	// later pause once its (older) flush resolves.
	let latestIntent = 0;
	return {
		async play() {
			const intent = ++latestIntent;
			// play() before the flush targets the old source, and the src swap
			// that follows aborts it: the new track would load silent.
			await flush();
			if (intent !== latestIntent) return;
			try {
				// Synthetic demo bytes may not decode in every engine; a rejected
				// play() must never break queue navigation.
				await audio()?.play();
			} catch {
				// Intentionally ignored.
			}
		},
		pause() {
			++latestIntent;
			audio()?.pause();
		},
		async unload() {
			const intent = ++latestIntent;
			audio()?.pause();
			// Removing `src` does not stop a loaded element; load() on the
			// emptied element releases it. Needs the flush so `src` is gone first.
			await flush();
			if (intent !== latestIntent) return;
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
