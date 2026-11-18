export function createMediaController() {
	let current: { audio: HTMLAudioElement } | undefined;
	let destroyed = false;

	function pause(audio: HTMLAudioElement | undefined): void {
		if (audio && !audio.paused) audio.pause();
	}

	function reset(): void {
		const audio = current?.audio;
		current = undefined;
		pause(audio);
	}

	return {
		get audio() { return current?.audio; },
		attach(audio: HTMLAudioElement): () => void {
			if (destroyed) return () => {};
			const previous = current?.audio;
			// Node identity is not attachment identity, even for the same source.
			const receipt = { audio };
			current = receipt;
			if (previous !== audio) pause(previous);
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
