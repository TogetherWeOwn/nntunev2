/** Source-scoped transport shared by the app player and its UI controls. */
import { createPlayback } from './playback.js';
import {
	applyMediaSession,
	clearMediaSession,
	setMediaSessionPosition,
	type ApplyMediaSessionOptions,
	type MediaSessionSeekDetails
} from './mediaSession.js';
import { createPlaybackSpeed } from './playbackSpeed.js';
import { createResumeStore } from './resume.js';
import {
	applySoundPreference,
	loadSoundPreference,
	saveSoundPreference,
	type SoundPreference
} from './soundPreference.js';
import { createSleepTimer, type SleepTimerMode, type SleepTimerProgress } from './sleepTimer.js';
import type { Track } from './queue.js';

export interface TransportSnapshot {
	status: 'selected' | 'loading' | 'playing' | 'paused' | 'buffering' | 'error';
	position: number;
	duration: number | null;
	sound: SoundPreference & { restricted?: boolean };
	rate: number;
	sleep: SleepTimerProgress | null;
}

export interface TransportDeps {
	storage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
	onchange: (snapshot: TransportSnapshot) => void;
	onnext: () => void;
	onprevious: () => void;
	/** Route system Play through any pending source-navigation intent. */
	onplay?: () => void;
	/** Cancel queued session playback, including a pause of an already-paused source. */
	onpause?: () => void;
	/** Only a verified natural completion, never a seek or explicit Next. */
	onended: (trackId: string) => void;
	now?: () => number;
	mediaSession?: ApplyMediaSessionOptions;
}

export function createTransport(deps: TransportDeps) {
	let element: HTMLAudioElement | null = null;
	let track: Track | null = null;
	let release = () => {};
	let generation = 0;
	let played = false;
	let completed = false;
	let terminalSeek = false;
	let pendingResume: number | null = null;
	let restoringPosition = false;
	let status: TransportSnapshot['status'] = 'selected';
	let sound = loadSoundPreference(deps.storage);
	const resume = createResumeStore({ storage: deps.storage });
	const speed = createPlaybackSpeed({ storage: deps.storage, audio: () => liveElement() });
	const sleep = createSleepTimer({ now: deps.now ?? Date.now, onFire: () => pause() });
	let restricted = false;

	function liveElement(): HTMLAudioElement | null {
		return element && track && element.getAttribute('src') === track.streamUrl ? element : null;
	}

	function snapshot(): TransportSnapshot {
		const audio = liveElement();
		const duration = audio && Number.isFinite(audio.duration) && audio.duration > 0 ? audio.duration : null;
		return {
			status,
			position: audio && Number.isFinite(audio.currentTime) ? Math.max(0, audio.currentTime) : 0,
			duration,
			sound: { ...sound, restricted },
			rate: audio && Number.isFinite(audio.playbackRate) ? audio.playbackRate : speed.current(),
			sleep: sleep.progress
		};
	}

	function publish(): void {
		deps.onchange(snapshot());
	}

	function publishPosition(isCurrent?: () => boolean): void {
		const state = snapshot();
		if (isCurrent && !isCurrent()) return;
		setMediaSessionPosition(state.duration === null ? null : {
			duration: state.duration, position: state.position, playbackRate: state.rate
		}, deps.mediaSession);
		if (isCurrent) {
			if (isCurrent()) deps.onchange(state);
		} else publish();
	}

	const playback = createPlayback({
		audio: liveElement,
		flush: () => Promise.resolve(),
		onPlaybackFailure: () => { status = 'error'; publish(); },
		onBufferingChange: (buffering) => {
			if (buffering) status = 'buffering';
			else if (status === 'buffering') status = liveElement()?.paused ? 'paused' : 'playing';
			publish();
		}
	});

	function persistPosition(): void {
		const audio = liveElement();
		if (audio && track && !completed && pendingResume === null) resume.save(track.id, audio.currentTime);
	}

	function setSound(next: SoundPreference): void {
		const audio = liveElement();
		if (!audio || !Number.isFinite(next.volume)) return;
		const applied = applySoundPreference(audio, {
			volume: Math.max(0, Math.min(1, next.volume)), muted: next.muted
		});
		sound = { volume: applied.volume, muted: applied.muted };
		restricted = applied.restricted;
		saveSoundPreference(deps.storage, sound);
		publish();
	}

	function cancelPendingResume(): void {
		pendingResume = null;
	}

	function observeSeek(isCurrent?: () => boolean): void {
		// Native controls supersede resume too. Ignore events emitted inside
		// our own restore setter; a refused restore must remain retryable.
		if (!restoringPosition) cancelPendingResume();
		const audio = liveElement();
		const terminal = !!audio && Number.isFinite(audio.duration) && audio.duration > 0 &&
			audio.currentTime >= audio.duration;
		if (!isCurrent || isCurrent()) terminalSeek = terminal;
	}

	function seek(seconds: number): boolean {
		const audio = liveElement();
		if (!audio || !Number.isFinite(seconds) || !Number.isFinite(audio.duration) || audio.duration <= 0) return false;
		return setPosition(audio, Math.max(0, Math.min(audio.duration, seconds)));
	}

	function seekToStart(boundary?: import('./nativeRestart.js').NativeResetBoundary): boolean {
		const audio = liveElement();
		// Zero is meaningful even before metadata, unlike an arbitrary seek.
		return audio ? setPosition(audio, 0, boundary) : false;
	}

	function setPosition(audio: HTMLAudioElement, target: number, boundary?: import('./nativeRestart.js').NativeResetBoundary): boolean {
		const revision = generation;
		const current = () => {
			if (!boundary) return true;
			if (revision !== generation || !boundary.isCurrent()) return false;
			const matching = liveElement() === audio;
			return matching && revision === generation && boundary.isCurrent();
		};
		// An explicit seek supersedes a deferred restore, even if the setter refuses it.
		pendingResume = null;
		if (!current()) return false;
		try {
			audio.currentTime = target;
		} catch {
			if (current()) {
				if (boundary) {
					const state = snapshot();
					if (current()) deps.onchange(state);
				} else publish();
			}
			return false;
		}
		if (!current()) return false;
		boundary?.onAssigned();
		if (!current()) return false;
		observeSeek(boundary ? current : undefined);
		if (!current()) return false;
		publishPosition(boundary ? current : undefined);
		if (!current()) return false;
		const readback = audio.currentTime;
		return current() && Math.abs(readback - target) < 0.01;
	}

	function restorePosition(): void {
		const audio = liveElement();
		if (!audio || pendingResume === null || !Number.isFinite(audio.duration) || audio.duration <= 0) return;
		const saved = pendingResume;
		if (saved >= audio.duration) {
			pendingResume = null;
			return;
		}
		try {
			restoringPosition = true;
			audio.currentTime = saved;
			if (Math.abs(audio.currentTime - saved) < 0.01) pendingResume = null;
		} catch {
			// Metadata may arrive before the resource is seekable; canplay retries.
		} finally {
			restoringPosition = false;
		}
	}

	async function play(): Promise<void> {
		const audio = liveElement();
		if (!audio || (!audio.paused && !audio.ended)) return;
		completed = false;
		status = 'loading';
		publish();
		await playback.play();
	}

	async function retry(): Promise<void> {
		const audio = liveElement();
		if (!audio) return;
		if (audio.error) {
			// play() alone never refetches a resource in a terminal media error.
			// Retire any pending play before load(), but keep this source's
			// callbacks and deferred resume. Pause/source replacement still
			// invalidate the new play through the normal playback intent fence.
			playback.pause();
			played = false;
			if (pendingResume === null && Number.isFinite(audio.currentTime) && audio.currentTime > 0) {
				pendingResume = audio.currentTime;
			}
			try {
				audio.load();
				// load() restores defaultPlaybackRate; reapply before queued
				// ratechange events can adopt and persist that reset as user intent.
				speed.apply();
			} catch {
				status = 'error';
				publish();
				return;
			}
		}
		await play();
	}

	function pause(): void {
		deps.onpause?.();
		played = false;
		playback.pause();
		if (element) status = 'paused';
		persistPosition();
		publish();
	}

	function systemSeek(details: MediaSessionSeekDetails, direction: -1 | 0 | 1): void {
		const audio = liveElement();
		if (!audio) return;
		if (direction === 0) {
			if (typeof details.seekTime === 'number') seek(details.seekTime);
		} else {
			const offset = details.seekOffset ?? 10;
			if (Number.isFinite(offset) && offset >= 0) seek(audio.currentTime + direction * offset);
		}
	}

	/** Account boundaries discard position; route changes keep this transport mounted. */
	function disconnect(savePosition = true): void {
		if (savePosition) persistPosition();
		else sleep.cancel();
		++generation;
		// Retire callbacks before pause, which can synchronously dispatch an event.
		release();
		release = () => {};
		playback.pause();
		element = null;
		track = null;
		played = false;
		pendingResume = null;
		status = 'selected';
		clearMediaSession(deps.mediaSession);
		publish();
	}

	function connect(audio: HTMLAudioElement, current: Track): () => void {
		disconnect();
		element = audio;
		track = current;
		const revision = generation;
		const isCurrent = () => revision === generation && liveElement() === audio;
		played = liveElement() === audio && !audio.paused && !audio.ended && !audio.error;
		if (liveElement() === audio && audio.error) status = 'error';
		else if (played) status = audio.readyState >= 3 ? 'playing' : 'loading';
		// An active source already has a live position; do not rewind to storage.
		pendingResume = played ? null : resume.positionFor(current.id);
		completed = false;
		terminalSeek = false;
		// No verified production loudness payload: additional leveling is unity.
		// Do not derive tags from unknown extras or create a graph to apply 1.
		const applied = applySoundPreference(audio, sound);
		sound = { volume: applied.volume, muted: applied.muted };
		restricted = applied.restricted;
		speed.apply();
		const stopObserving = playback.observe(audio);
		const syncTime = () => { restorePosition(); publishPosition(); };
		const events: [string, () => void][] = [
			['play', () => { if (!audio.paused) { played = true; completed = false; status = 'loading'; } publish(); }],
			['playing', () => {
				if (!audio.paused) {
					played = true;
					completed = false;
					if (audio.currentTime < audio.duration) terminalSeek = false;
					status = 'playing';
				}
				publish();
			}],
			['pause', () => {
				if (!audio.ended) {
					if (played) deps.onpause?.();
					played = false;
					status = 'paused';
					persistPosition();
					publish();
				}
			}],
			['error', () => { played = false; status = 'error'; publish(); }],
			['ended', () => {
				if (!played || completed || !audio.ended || audio.error) return;
				if (terminalSeek) {
					played = false;
					status = 'paused';
					publish();
					return;
				}
				completed = true;
				played = false;
				pendingResume = null;
				resume.clear(current.id);
				status = 'paused';
				const sleeping = sleep.trackEnded();
				publish();
				if (!sleeping) deps.onended(current.id);
			}],
			['loadedmetadata', syncTime], ['durationchange', syncTime], ['canplay', syncTime],
			['timeupdate', publishPosition], ['seeking', observeSeek],
			['seeked', () => { observeSeek(); publishPosition(); }],
			['ratechange', () => { speed.adoptNativeRate(audio.playbackRate); publishPosition(); }],
			['volumechange', () => {
				sound = { volume: audio.volume, muted: audio.muted };
				saveSoundPreference(deps.storage, sound);
				publish();
			}]
		];
		const listeners = events.map(([name, handler]) => {
			const guarded = () => { if (isCurrent()) handler(); };
			audio.addEventListener(name, guarded);
			return [name, guarded] as const;
		});
		release = () => {
			stopObserving();
			for (const [name, listener] of listeners) audio.removeEventListener(name, listener);
		};
		const guard = (action: () => void) => () => { if (isCurrent()) action(); };
		applyMediaSession(current, {
			onPlay: guard(() => deps.onplay ? deps.onplay() : void play()), onPause: guard(pause),
			onNext: guard(deps.onnext), onPrevious: guard(deps.onprevious),
			onSeekTo: (details) => { if (isCurrent()) systemSeek(details, 0); },
			onSeekForward: (details) => { if (isCurrent()) systemSeek(details, 1); },
			onSeekBackward: (details) => { if (isCurrent()) systemSeek(details, -1); }
		}, deps.mediaSession);
		restorePosition();
		publishPosition();
		return () => { if (revision === generation) disconnect(); };
	}

	return {
		connect, disconnect, snapshot, play, retry, pause, seek, seekToStart, cancelPendingResume, persistPosition,
		seekBy: (seconds: number) => {
			const audio = liveElement();
			return audio && Number.isFinite(audio.currentTime) && Number.isFinite(seconds)
				? seek(audio.currentTime + seconds) : false;
		},
		togglePlay: () => {
			if (status === 'loading' || status === 'playing' || status === 'buffering') pause();
			else void play();
		},
		setVolume: (volume: number) => setSound({ volume, muted: false }),
		toggleMute: () => setSound({ ...sound, muted: !sound.muted }),
		setSpeed: (rate: number) => { speed.set(rate); publishPosition(); },
		armSleep: (mode: SleepTimerMode) => { sleep.start(mode); publish(); },
		cancelSleep: () => { sleep.cancel(); publish(); },
		tick: () => { sleep.tick(); publish(); },
		destroy: () => { sleep.cancel(); disconnect(); }
	};
}

export type Transport = ReturnType<typeof createTransport>;
