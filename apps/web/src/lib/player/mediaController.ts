import { selectTrack, type QueueState } from './queue.js';
import type { createQueueController, QueuedPlaySource } from './queueController.js';

export type MediaAttachment = { readonly audio: HTMLAudioElement; playing: boolean };

type NativeObservations = {
	source: () => string | null;
	interrupt: () => void;
	ended: (attachment: MediaAttachment) => Promise<void>;
	seeking: (audio: HTMLAudioElement) => void;
	metadata: (audio: HTMLAudioElement) => void;
	position: (audio: HTMLAudioElement) => void;
};

type PlaySelection = {
	revision: Pick<ReturnType<typeof createQueueController>, 'revoke' | 'isCurrent'>;
	commit: (queue: QueueState) => boolean;
	flush: () => Promise<void>;
	announce: (message: string) => void;
	bumpAnnouncement: () => void;
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

	async function playSelected(
		base: QueueState, index: number, source: QueuedPlaySource, suffix: string, deps: PlaySelection
	): Promise<void> {
		const target = base.tracks[index];
		if (!target || destroyed || !source.isCurrent()) return;
		const { streamUrl } = source;
		const tracks = [...base.tracks];
		tracks[index] = { ...target, streamUrl };
		if (!deps.commit(selectTrack({ ...base, tracks }, index)) || !source.isCurrent() || destroyed) return;

		const receipt = deps.revision.revoke();
		let attachment = current;
		const ownsAttempt = () => !destroyed && deps.revision.isCurrent(receipt) && source.isCurrent();
		const isCurrent = () => ownsAttempt() && current === attachment;
		const title = Array.from(target.title.trim() || target.id).slice(0, 60).join('');
		let lead = `Selected ${title}; no stream to play.`;
		if (streamUrl) {
			await deps.flush();
			// A source change retires the old node before commit. In that case
			// flush mounts its replacement; a surviving node keeps its receipt.
			if (!ownsAttempt() || (attachment && !isCurrent())) return;
			attachment = current;
			const audio = attachment?.audio;
			lead = `Selected ${title}, paused: playback was blocked.`;
			try {
				if (audio?.isConnected && audio.getAttribute('src') === streamUrl && isCurrent()) {
					const play = audio.play;
					if (!isCurrent()) return;
					await play.call(audio);
					lead = `Playing ${title}.`;
				}
			} catch {
				// No retry and no pause or selection change on a retired receipt.
				if (isCurrent() && attachment) attachment.playing = false;
			}
		}
		if (!isCurrent()) return;
		deps.announce(suffix ? `${lead} ${suffix}` : lead);
		if (isCurrent()) deps.bumpAnnouncement();
	}

	return {
		playSelected,
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
