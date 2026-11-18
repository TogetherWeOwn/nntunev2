export type MediaAttachment = { readonly audio: HTMLAudioElement; playing: boolean };

type NativeObservations = {
	source: () => string | null;
	interrupt: () => void;
	ended: (attachment: MediaAttachment) => Promise<void>;
	seeking: (audio: HTMLAudioElement) => void;
	metadata: (audio: HTMLAudioElement) => void;
	position: (audio: HTMLAudioElement) => void;
};

// Queued native events belong to the element, not to a receipt or controller.
const mountedAudio = new WeakSet<HTMLAudioElement>();

export function createMediaController() {
	let current: (MediaAttachment & { listeners: AbortController }) | undefined;
	let destroyed = false;

	function pause(audio: HTMLAudioElement | undefined): void {
		if (audio && !audio.paused) audio.pause();
	}

	function reset(): void {
		const previous = current;
		current = undefined;
		previous?.listeners.abort();
		pause(previous?.audio);
	}

	return {
		get audio() { return current?.audio; },
		get attachment(): MediaAttachment | undefined { return current; },
		attach(audio: HTMLAudioElement, observations?: NativeObservations): () => void {
			if (destroyed) return () => {};
			if (observations && mountedAudio.has(audio)) throw new Error('Native observations require a fresh audio element');
			mountedAudio.add(audio);
			const previous = current;
			// Node identity is not attachment identity, even for the same source.
			const receipt = { audio, playing: false, listeners: new AbortController() };
			current = receipt;
			previous?.listeners.abort();
			if (observations) {
				const observe = (event: Event) => {
					if (current !== receipt || event.currentTarget !== audio || !audio.isConnected) return;
					const source = observations.source();
					if (!source || audio.getAttribute('src') !== source) return;
					switch (event.type) {
						case 'play':
						case 'playing':
							if (!audio.paused && !audio.ended) receipt.playing = true;
							return;
						case 'pause':
							// Native completion pauses before `ended`; retain that intent.
							if (!audio.paused || audio.ended) return;
							break;
						case 'error':
							if (audio.error === null) return;
							break;
						case 'ended': return observations.ended(receipt);
						case 'seeking': return observations.seeking(audio);
						case 'loadedmetadata':
						case 'durationchange':
							observations.metadata(audio);
							if (current !== receipt) return;
							// fall through to refresh position availability
						default: return observations.position(audio);
					}
					receipt.playing = false;
					observations.interrupt();
				};
				for (const type of ['play', 'playing', 'pause', 'error', 'ended', 'seeking',
					'loadedmetadata', 'durationchange', 'canplay', 'progress', 'timeupdate']) {
					audio.addEventListener(type, observe, { signal: receipt.listeners.signal });
				}
			}
			if (previous?.audio !== audio) pause(previous?.audio);
			return () => {
				if (current === receipt) reset();
			};
		},
		reset,
		destroy() {
			destroyed = true;
			reset();
		}
	};
}
