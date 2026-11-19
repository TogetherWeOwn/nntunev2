import { selectTrack, type QueueState } from './queue.js';
import type { createQueueController, QueuedPlaySource } from './queueController.js';
import { jumpMedia, type JumpDirection, type JumpOutcome } from './relativeJump.js';

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

type JumpSelection = {
	isCurrent: () => boolean;
	supersedeShare: () => void;
	assigned: () => void;
	publish: (outcome: JumpOutcome) => void;
};

type RestartSelection = {
	supersedeShare: () => () => boolean;
	available: (audio: HTMLAudioElement, matches: () => boolean) => boolean;
	assigned: () => void;
	publish: (message: string, available: boolean) => void;
};

function describeRestart(words: string, title: string): string {
	const suffix = ` ${title}.`;
	if (words.length + suffix.length <= 99) return `${words}${suffix}`;
	let end = 97 - words.length;
	// Never cut between the halves of a surrogate pair.
	const last = title.charCodeAt(end - 1);
	if (last >= 0xd800 && last <= 0xdbff) end -= 1;
	return `${words} ${title.slice(0, end)}…`;
}

// Queued native events belong to the element, not to a receipt or controller.
const mountedAudio = new WeakSet<HTMLAudioElement>();
const nativeIsConnected = typeof Node === 'undefined' ? undefined : Object.getOwnPropertyDescriptor(Node.prototype, 'isConnected')?.get;
const nativeGetAttribute = typeof Element === 'undefined' ? undefined : Element.prototype.getAttribute;

function matchesNativeSource(audio: HTMLAudioElement, source: string): boolean {
	return nativeIsConnected?.call(audio) === true && nativeGetAttribute?.call(audio, 'src') === source;
}

export function createMediaController(matchesNative = matchesNativeSource) {
	let current: (MediaAttachment & { listeners: AbortController }) | undefined;
	let destroyed = false;
	let positionIntent = 0;
	let positioning = false;
	let nextPosition: (() => void) | undefined;

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

	function matchesSource(audio: HTMLAudioElement, source: string, isCurrent: () => boolean): boolean {
		try {
			// Either instance accessor can change what the other last observed. End
			// with a DOM snapshot that does not call back into those accessors.
			return isCurrent() && audio.isConnected && isCurrent() &&
				audio.getAttribute('src') === source && isCurrent() && audio.isConnected && isCurrent() &&
				matchesNative(audio, source);
		} catch {
			return false;
		}
	}

	function runPosition(action: () => void): void {
		// A setter can call a newer gesture before storing its own value. Revoke
		// the old intent immediately, but let the native call unwind before writing again.
		nextPosition = action;
		if (positioning) return;
		positioning = true;
		try {
			while (nextPosition) {
				const run = nextPosition;
				nextPosition = undefined;
				run();
			}
		} finally {
			positioning = false;
			nextPosition = undefined;
		}
	}

	function jump(source: string, direction: JumpDirection, deps: JumpSelection): void {
		if (destroyed) return;
		const attachment = current;
		const audio = attachment?.audio;
		const intent = ++positionIntent;
		const isCurrent = () => !destroyed && current === attachment && positionIntent === intent && deps.isCurrent();
		// Cancel the older share on acceptance, not when the native seek drains.
		if (audio && source && isCurrent() && matchesNative(audio, source)) deps.supersedeShare();
		runPosition(() => {
			if (!audio || !source || !matchesSource(audio, source, isCurrent)) {
				if (isCurrent()) deps.publish({ kind: 'unavailable' });
				return;
			}
			const matches = () => matchesSource(audio, source, isCurrent);
			if (!matches()) return;
			const outcome = jumpMedia(audio, direction, matches, () => {
				if (current === attachment) deps.assigned();
			});
			if (matches()) deps.publish(outcome);
		});
	}

	function restart(source: string, title: string, deps: RestartSelection): void {
		if (destroyed) return;
		const attachment = current;
		const intent = ++positionIntent;
		// Supersede before any native read, including an unavailable or refused seek.
		const selectionIsCurrent = deps.supersedeShare();
		if (!attachment || positionIntent !== intent) return;
		const { audio } = attachment;
		const isCurrent = () => current === attachment && positionIntent === intent && selectionIsCurrent();
		const matches = () => matchesSource(audio, source, isCurrent);
		runPosition(() => {
			if (!matches()) return;

			let words = 'Could not restart';
			let available = false;
			try {
				available = deps.available(audio, matches);
				if (available && matches()) {
					const position = audio.currentTime;
					if (Number.isFinite(position) && matches()) {
						const atStart = Math.abs(position) <= 0.05;
						if (atStart) words = 'Already at the start of';
						audio.currentTime = 0;
						// Queue edits can revoke readback without retiring the old position feedback.
						if (current === attachment) deps.assigned();
						if (!atStart) {
							if (!matches()) return;
							const readback = audio.currentTime;
							if (Number.isFinite(readback) && Math.abs(readback) <= 0.05) words = 'Restarted';
						}
					}
				}
			} catch {
				// Unreadable positions and refused assignments cannot prove a restart.
			}
			if (matches()) deps.publish(describeRestart(words, title), available);
		});
	}

	return {
		playSelected,
		restart,
		jump,
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
