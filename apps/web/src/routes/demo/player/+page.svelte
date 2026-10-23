<svelte:head>
	<title>NNTune — player resilience demo</title>
	<meta name="description" content="Seek-storm and artwork-fallback demo for player smoke tests." />
</svelte:head>

<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { page } from '$app/state';
	import Artwork from '#lib/player/Artwork.svelte';
	import LevelingToggle from '#lib/player/LevelingToggle.svelte';
	import {
		applyGainToNode,
		loadLevelingEnabled,
		resolveTrackLinearGain,
		saveLevelingEnabled,
		type LevelingTags
	} from '#lib/player/replayGain.js';
	import { SeekController } from '#lib/player/seekController.js';
	import { applyMediaSession, clearMediaSession } from '#lib/player/mediaSession.js';
	import { createPlayback, endedForCurrent } from '#lib/player/playback.js';
	import { createRadioOrder, parseRadioSeed } from '#lib/player/radio.js';
	import { createPlaybackSpeed, type PlaybackSpeed } from '#lib/player/playbackSpeed.js';
	import { createResumeStore, type ResumeStore } from '#lib/player/resume.js';
	import { buildShareQuery, parseTimestampParam } from '#lib/player/shareTimestamp.js';
	import { createSleepTimer, type SleepTimerMode } from '#lib/player/sleepTimer.js';
	import {
		actionForKey,
		isModalOpen,
		isTypingTarget,
		SEEK_SECONDS,
		SHORTCUT_ROWS
	} from '#lib/player/shortcuts.js';
	import {
		addTrack,
		clearQueue,
		createQueue,
		currentTrack,
		DEMO_TRACKS,
		moveTrack,
		nextTrack,
		previousTrack,
		restoreQueue,
		selectTrack,
		serializeQueue,
		type QueueState
	} from '#lib/player/queue.js';

	const AUDIO_URL = '/api/demo/audio';
	const QUEUE_STORAGE_KEY = 'nntune:demo-queue:v1';

	/**
	 * Fixture ReplayGain tags for the demo tracks: synthetic metadata for
	 * synthetic audio (no backend). Peaks keep the clipping guard inert by
	 * construction, so the gain node pins leveling, not limiting.
	 */
	const DEMO_LEVELING_TAGS: Readonly<Record<string, LevelingTags>> = {
		'demo-track': { trackGainDb: '-3.00 dB', trackPeak: 0.42 },
		'second-signal': { trackGainDb: '+4.00 dB', trackPeak: 0.3 },
		'third-light': { trackGainDb: '-1.50 dB', trackPeak: 0.48 }
	};

	type TransportState = 'selected' | 'loading' | 'playing' | 'paused' | 'error' | 'ended' | 'empty';
	let status = $state<TransportState>('selected');
	let seekReady = $state(false);
	let sourceRevision = $state(0);
	let completedSeeks = $state(0);
	let abortedSeeks = $state(0);
	let lastBytes = $state(0);

	let queue: QueueState = $state(createQueue(DEMO_TRACKS));
	let restored = $state(false);
	let audioEl: HTMLAudioElement | undefined = $state();
	let muted = $state(false);
	let shortcutsOpen = $state(false);
	let shortcutsDialogEl: HTMLDialogElement | undefined = $state();
	let shortcutsOpener: HTMLElement | null = null;

	/**
	 * Auto-continue radio: a seeded pass over the loaded catalogue that
	 * starts on natural end of the queue. Null when radio is off. The queue
	 * is untouched while radio plays, so stopping returns to the queue's
	 * end state instead of a shuffled one.
	 */
	interface RadioState {
		order: (typeof DEMO_TRACKS)[number][];
		index: number;
		seed: number;
	}
	let radio = $state<RadioState | null>(null);

	const current = $derived(radio ? (radio.order[radio.index] ?? null) : currentTrack(queue));
	const inQueue = $derived(new Set(queue.tracks.map((t) => t.id)));

	// Copy link at current time (synthetic demo ids only): explicit button
	// activation copies a fresh same-origin `/demo/player?track=&t=` URL
	// built with buildShareQuery. No clipboard access happens on load,
	// track change, seek, or timeupdate.
	const SHARE_COPY_DONE = 'Copied link at current time.';
	let shareCopyStatus = $state('');
	let shareCopyFallbackUrl = $state<string | null>(null);
	// Bumped on every activation and every track/source switch so a late
	// clipboard settlement from a superseded action is ignored.
	let shareCopyGeneration = 0;
	let shareCopyAlive = true;
	const shareCopyEnabled = $derived(
		current !== null && DEMO_TRACKS.some((track) => track.id === (current?.id ?? ''))
	);

	const playback = createPlayback({
		audio: () => audioEl,
		flush: tick,
		onPlaybackFailure: () => {
			status = 'error';
		},
		onPlaybackStarted: () => {
			status = 'playing';
		}
	});

	// Loudness leveling: the toggle persists across reloads (default ON) and
	// each track's fixture tags resolve to a gain-node multiplier.
	let levelingEnabled = $state(true);
	let levelingLinear = $state(1);
	let levelingCtx: AudioContext | null = null;
	let levelingSource: MediaElementAudioSourceNode | null = null;
	let levelingGain: GainNode | null = null;
	let wiredEl: HTMLAudioElement | null = null;

	function safeStorage(): Storage | null {
		try {
			return localStorage;
		} catch {
			return null;
		}
	}

	/**
	 * One AudioContext per page; each keyed `<audio>` element gets its own
	 * MediaElementSource (an element can only be captured once). Leveling
	 * failure never breaks playback: unwired elements play at natural volume.
	 */
	function ensureLevelingGraph(): GainNode | null {
		try {
			if (typeof AudioContext === 'undefined' || !audioEl) return null;
			if (wiredEl !== audioEl) {
				levelingSource?.disconnect();
				levelingGain?.disconnect();
				levelingCtx ??= new AudioContext();
				levelingSource = levelingCtx.createMediaElementSource(audioEl);
				levelingGain = levelingCtx.createGain();
				levelingSource.connect(levelingGain);
				levelingGain.connect(levelingCtx.destination);
				wiredEl = audioEl;
			}
			return levelingGain;
		} catch {
			return null;
		}
	}

	function applyLeveling(): void {
		const tags = (current && DEMO_LEVELING_TAGS[current.id]) ?? {};
		const linear = resolveTrackLinearGain(tags, { enabled: levelingEnabled });
		levelingLinear = linear;
		const node = ensureLevelingGraph();
		if (node) {
			applyGainToNode(node, linear);
			resumeLevelingContext();
		}
	}

	/**
	 * Best-effort resume of the page-local leveling context. The mount-time
	 * resume can reject under autoplay policy (no user activation yet), and
	 * a suspended context renders routed playback silent — so every play
	 * path retries it inside the user's gesture.
	 */
	function resumeLevelingContext(): void {
		if (!levelingCtx || levelingCtx.state === 'running') return;
		void levelingCtx.resume().catch(() => {
			// Still suspended; the next user-initiated play retries.
		});
	}

	function setLevelingEnabled(enabled: boolean): void {
		levelingEnabled = enabled;
		saveLevelingEnabled(safeStorage(), enabled);
	}

	function playCurrent(reload = false): void {
		if (!current) return;
		// A play press is a user gesture: retry the mount-time resume so a
		// suspended leveling context cannot mute this playback.
		resumeLevelingContext();
		// A failed media resource needs a fresh load even when its URL is
		// unchanged. A keyed element also fences queued events from that load.
		if (reload || status === 'error') sourceRevision += 1;
		status = 'loading';
		void playback.play();
	}

	function pauseCurrent(): void {
		playback.pause();
		if (current && status !== 'error' && status !== 'ended') status = 'paused';
	}

	/**
	 * Explicit-activation copy: reads the live audio element's time at click
	 * time and writes a fresh same-origin share URL (only `track` + `t`).
	 * Refuses silently for no recognized demo track or unusable media time.
	 * A missing/rejected clipboard shows a labeled readonly fallback URL
	 * instead of claiming success. Never touches transport, queue, or storage.
	 */
	async function copyShareLink(): Promise<void> {
		// Every activation supersedes prior results, even one that refuses
		// below: clear stale success/fallback state up front so an invalid
		// activation cannot leave a misleading success message or a live
		// pending write behind.
		const generation = (shareCopyGeneration += 1);
		shareCopyStatus = '';
		shareCopyFallbackUrl = null;
		const trackId = current?.id ?? null;
		if (trackId === null || !DEMO_TRACKS.some((track) => track.id === trackId)) return;
		const position = audioEl?.currentTime;
		if (typeof position !== 'number' || !Number.isFinite(position) || position < 0) return;
		let query: string;
		try {
			query = buildShareQuery(trackId, position);
		} catch {
			return;
		}
		const url = `${window.location.origin}/demo/player?${query}`;
		const revision = sourceRevision;
		type ClipboardWriter = { writeText: (text: string) => Promise<void> };
		let clipboard: ClipboardWriter | undefined;
		try {
			const candidate = (navigator as Navigator & { clipboard?: ClipboardWriter }).clipboard;
			clipboard = typeof candidate?.writeText === 'function' ? candidate : undefined;
		} catch {
			clipboard = undefined;
		}
		if (!clipboard) {
			shareCopyStatus = '';
			shareCopyFallbackUrl = url;
			return;
		}
		try {
			await clipboard.writeText(url);
		} catch {
			// Denied or failed write: same fallback, no success claim.
			if (!shareCopyAlive || generation !== shareCopyGeneration) return;
			if (current?.id !== trackId || sourceRevision !== revision) return;
			shareCopyStatus = '';
			shareCopyFallbackUrl = url;
			return;
		}
		if (!shareCopyAlive || generation !== shareCopyGeneration) return;
		if (current?.id !== trackId || sourceRevision !== revision) return;
		shareCopyFallbackUrl = null;
		shareCopyStatus = SHARE_COPY_DONE;
	}

	// Player comforts: sleep timer, playback speed, per-track resume.
	let sleepMode = $state<SleepTimerMode | null>(null);
	let sleepAnnouncement = $state('Sleep timer off');
	let minutesEl: HTMLInputElement | undefined = $state();
	let tracksEl: HTMLInputElement | undefined = $state();
	let speedValue = $state('1');
	let speed: PlaybackSpeed | null = null;
	let resumeStore: ResumeStore | null = null;

	// The test hook advances the same clock the live interval ticks, so
	// duration paths stay deterministic without a second timer.
	let clockOffset = 0;
	const sleepTimer = createSleepTimer({
		now: () => Date.now() + clockOffset,
		onFire: () => {
			sleepMode = null;
			sleepAnnouncement = 'Sleep timer fired — playback paused';
			pauseCurrent();
		}
	});

	function armDuration(): void {
		const raw = minutesEl?.value.trim() ?? '';
		const minutes = Number(raw);
		if (!Number.isFinite(minutes) || minutes <= 0) return;
		sleepTimer.start({ kind: 'duration', ms: minutes * 60_000 });
		sleepMode = sleepTimer.mode;
		sleepAnnouncement = `Sleep in ${raw} minutes`;
	}

	function armEndOfTrack(): void {
		sleepTimer.start({ kind: 'end-of-track' });
		sleepMode = sleepTimer.mode;
		sleepAnnouncement = 'Sleep at end of track';
	}

	function armStopAfter(): void {
		const count = Number(tracksEl?.value.trim() ?? '');
		if (!Number.isInteger(count) || count < 1) return;
		sleepTimer.start({ kind: 'stop-after-tracks', count });
		sleepMode = sleepTimer.mode;
		sleepAnnouncement = `Sleep after ${count} tracks`;
	}

	function cancelSleep(): void {
		sleepTimer.cancel();
		sleepMode = null;
		sleepAnnouncement = 'Sleep timer off';
	}

	/** Drive the real armed duration timer forward; the smoke-test hook. */
	function advanceSleep(ms: number): void {
		clockOffset += ms;
		sleepTimer.tick();
	}

	function handleSpeedChange(value: string): void {
		speedValue = value;
		speed?.set(Number(value));
	}

	/** In-memory Storage stand-in when the browser blocks site storage. */
	function memoryStorage(): Pick<Storage, 'getItem' | 'setItem' | 'removeItem'> {
		const entries = new Map<string, string>();
		return {
			getItem: (key) => entries.get(key) ?? null,
			setItem: (key, value) => {
				entries.set(key, String(value));
			},
			removeItem: (key) => {
				entries.delete(key);
			}
		};
	}

	/** Persist the outgoing track's latest position (zero means never started). */
	function persistPosition(
		trackId: string | undefined,
		element: HTMLAudioElement | undefined = audioEl
	): void {
		if (!trackId || !resumeStore) return;
		// An ended node carries the finished position: persisting it would
		// resurrect the entry handleEnded just cleared.
		if (element?.ended) return;
		const position = element?.currentTime;
		if (typeof position !== 'number' || !Number.isFinite(position) || position <= 0) return;
		resumeStore.save(trackId, position);
	}

	/** Seek to a track's saved resume position after the element swap lands. */
	function seekToResume(trackId: string): void {
		void tick().then(() => {
			if (current?.id !== trackId) return;
			const element = audioEl;
			if (!element || !resumeStore) return;
			const saved = resumeStore.positionFor(trackId);
			if (saved === null) return;
			try {
				element.currentTime = saved;
			} catch {
				// The element is not seekable yet; keep playing from the start.
			}
		});
	}

	function seekCurrentResume(): void {
		if (current) seekToResume(current.id);
	}

	// A source gets its own element: an event queued by the browser before
	// a src swap otherwise has no source/intent identifier we could inspect.
	function releaseAudio(node: HTMLAudioElement, trackId: string | undefined) {
		return {
			destroy() {
				// The action tears down on every swap and on unmount, before
				// the source unloads: persist from this node while it holds time.
				persistPosition(trackId, node);
				node.pause();
				node.removeAttribute('src');
				node.load();
			}
		};
	}

	function eventForCurrent(event: Event): boolean {
		return event.currentTarget === audioEl &&
			endedForCurrent(event.currentTarget as HTMLAudioElement, current?.streamUrl);
	}

	function handlePlay(event: Event): void {
		// A queued play event can arrive after a newer pause. The element's
		// current paused flag distinguishes that from an explicit native resume.
		if (!eventForCurrent(event) || (event.currentTarget as HTMLAudioElement).paused) return;
		// Native controls bypass playCurrent: retry the resume on the gesture.
		resumeLevelingContext();
		if (status === 'error') playCurrent(true);
		else if (status !== 'playing') status = 'loading';
	}

	function handlePlaying(event: Event): void {
		if (!eventForCurrent(event)) return;
		// Media Session and programmatic plays also land here; retry the
		// resume so routed playback is never stuck silent.
		resumeLevelingContext();
		if (status === 'loading' || status === 'playing') status = 'playing';
	}

	function handlePause(event: Event): void {
		// Natural completion fires pause before ended; let handleEnded advance
		// the queue instead of treating that pause as a newer user intent.
		if ((event.currentTarget as HTMLAudioElement).ended) return;
		if (eventForCurrent(event) && (status === 'loading' || status === 'playing')) pauseCurrent();
	}

	function handleError(event: Event): void {
		if (!eventForCurrent(event) || status === 'ended' || status === 'empty') return;
		// Invalidate any still-pending play fulfillment before exposing error.
		playback.pause();
		status = 'error';
	}

	function goNext(): void {
		// Explicit Next during radio moves through the mix (rolling into a
		// new pass at its end). Only explicit Next past the queue end stops.
		if (radio) {
			advanceRadio();
			return;
		}
		if (!current) return;
		const advanced = nextTrack(queue);
		if (advanced === queue) {
			// End of the queue: really stop, so status and playback agree.
			playback.pause();
			status = 'ended';
			return;
		}
		queue = advanced;
		playCurrent();
	}

	// Natural end of the last queued track: continue with a seeded mix over
	// the loaded catalogue. An empty library ends instead of starting radio.
	function startRadio(): void {
		startRadioPass(parseRadioSeed(page.url.searchParams.get('radio-seed')));
	}

	function startRadioPass(seed: number): void {
		const order = createRadioOrder(DEMO_TRACKS, seed);
		if (order.length === 0) {
			playback.pause();
			status = 'ended';
			return;
		}
		radio = { order, index: 0, seed };
		playCurrent();
	}

	// Advance within the radio mix; a finished pass rolls into a new one
	// with the next seed, so radio never idles mid-listen.
	function advanceRadio(): void {
		if (!radio) return;
		if (radio.index < radio.order.length - 1) {
			radio = { ...radio, index: radio.index + 1 };
			playCurrent();
		} else {
			startRadioPass(radio.seed + 1);
		}
	}

	// Stop returns to the established end-of-queue idle state. Nothing
	// restarts on its own: stray ends land on status 'ended', which
	// handleEnded ignores.
	function stopRadio(): void {
		radio = null;
		playback.pause();
		status = 'ended';
	}

	function goPrevious(): void {
		const previous = previousTrack(queue);
		// Clamped at the first track: nothing changed, so a paused track must
		// not be resumed by a no-op Previous (and radio, if on, keeps playing).
		if (previous === queue) return;
		// Stepping back leaves radio: the queue resumes from the previous track.
		radio = null;
		queue = previous;
		playCurrent();
	}

	// Explicit navigation persists the outgoing track's position and seeks
	// the incoming track to its saved one. Natural `ended` advances go
	// through goNext directly: the ended track is cleared, not persisted.
	function userNext(): void {
		const outgoingId = current?.id;
		persistPosition(outgoingId);
		goNext();
		if (current?.id !== outgoingId) seekCurrentResume();
	}

	function userPrevious(): void {
		const outgoingId = current?.id;
		persistPosition(outgoingId);
		goPrevious();
		if (current?.id !== outgoingId) seekCurrentResume();
	}

	function selectAt(index: number): void {
		const next = selectTrack(queue, index);
		const nextId = next.tracks[next.currentIndex]?.id;
		// Keep live progress on reselection; a never-started element can
		// still load a seeded resume entry under the selection contract.
		const shouldResume = nextId !== current?.id || !audioEl?.currentTime;
		if (nextId !== undefined && nextId !== current?.id) persistPosition(current?.id);
		// An explicit pick leaves radio: the queue resumes from the picked track.
		radio = null;
		queue = next;
		playCurrent();
		if (nextId !== undefined && shouldResume) seekToResume(nextId);
	}

	function addToQueue(track: (typeof DEMO_TRACKS)[number]): void {
		const wasEmpty = queue.tracks.length === 0;
		queue = addTrack(queue, track);
		// The first add to an empty queue makes it current: start it like any
		// other track change instead of showing a track that never plays.
		if (wasEmpty) {
			playCurrent();
			seekCurrentResume();
		}
	}

	function clearAll(): void {
		radio = null;
		queue = clearQueue(queue);
		status = 'empty';
		void playback.unload();
	}

	async function openShortcuts(): Promise<void> {
		if (shortcutsOpen) return;
		shortcutsOpener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
		shortcutsOpen = true;
		await tick();
		// Native modality moves focus to Close and makes the background inert.
		if (shortcutsOpen) shortcutsDialogEl?.showModal();
	}

	function closeShortcuts(): void {
		shortcutsDialogEl?.close();
		shortcutsOpen = false;
		shortcutsOpener?.focus();
	}

	function togglePlayShortcut(): void {
		if (status === 'playing' || status === 'loading') pauseCurrent();
		else playCurrent();
	}

	function seekShortcut(deltaSeconds: number): void {
		const el = audioEl;
		if (!el) return;
		const max = Number.isFinite(el.duration) ? el.duration : Number.POSITIVE_INFINITY;
		el.currentTime = Math.min(Math.max(0, el.currentTime + deltaSeconds), max);
	}

	// Mute lives in player state, not on the element: the keyed <audio>
	// is recreated on every track change, and native controls can mute
	// behind the shortcut's back. The effect re-applies state to each
	// new element; volumechange absorbs native-side changes.
	$effect(() => {
		if (audioEl) audioEl.muted = muted;
	});

	function handleVolumeChange(event: Event): void {
		if (event.currentTarget instanceof HTMLAudioElement) muted = event.currentTarget.muted;
	}

	function toggleMuteShortcut(): void {
		muted = !muted;
		if (audioEl) audioEl.muted = muted;
	}

	// Global transport keymap. Escape closes our dialog first so a modal
	// guard can never wedge it; every other binding yields to typing
	// surfaces, open modals and modified chords. No preventDefault: the
	// map holds only keys with no page-level default to suppress.
	function handleGlobalKeydown(event: KeyboardEvent): void {
		if (event.metaKey || event.ctrlKey || event.altKey) return;
		const action = actionForKey(event.key);
		if (!action) return;
		if (action === 'close-shortcuts') {
			if (shortcutsOpen) closeShortcuts();
			return;
		}
		if (shortcutsOpen || isTypingTarget(event.target) || isModalOpen(document)) return;
		switch (action) {
			case 'toggle-play':
				togglePlayShortcut();
				break;
			case 'next-track':
				goNext();
				break;
			case 'previous-track':
				goPrevious();
				break;
			case 'seek-back':
				seekShortcut(-SEEK_SECONDS);
				break;
			case 'seek-forward':
				seekShortcut(SEEK_SECONDS);
				break;
			case 'toggle-mute':
				toggleMuteShortcut();
				break;
			case 'open-shortcuts':
				openShortcuts();
				break;
		}
	}

	// `ended` is only honoured for the source the queue still points at; a
	// late event from a superseded track must not skip the new one.
	function handleEnded(event: Event): void {
		if (!eventForCurrent(event) || status === 'paused' || status === 'ended') return;
		const endedId = current?.id ?? null;
		const fired = sleepTimer.trackEnded();
		// A finished track never resumes mid-way, whether the timer fired or
		// the queue advances.
		if (endedId !== null) resumeStore?.clear(endedId);
		// A fired timer already paused via onFire; the queue stays untouched.
		if (fired) return;
		// Only a natural end starts radio: explicit Next past the end stops
		// in goNext, and pause (or a stale ended while paused) never gets here.
		if (radio) {
			advanceRadio();
		} else {
			if (!current) return;
			if (nextTrack(queue) === queue) startRadio();
			else goNext();
		}
		seekCurrentResume();
	}

	// MediaSession follows the current track; queue order + current track
	// persist for the tab session (sessionStorage, restored on mount).
	$effect(() => {
		applyMediaSession(current, {
			onNext: userNext,
			onPrevious: userPrevious,
			onPlay: () => playCurrent(),
			onPause: pauseCurrent
		});
		if (restored) {
			try {
				sessionStorage.setItem(QUEUE_STORAGE_KEY, serializeQueue(queue));
			} catch {
				// Storage can be unavailable (private mode); queue still works.
			}
		}
	});

	// Leveling follows the current track, the toggle, and the wired element
	// (the keyed `<audio>` is recreated per source, so a track change rewires).
	$effect(() => {
		applyLeveling();
	});

	// A new track or source invalidates any pending copy and clears its
	// outcome, so stale confirmations never linger past a switch.
	$effect(() => {
		const sourceIdentity = `${current?.id ?? ''}:${current?.streamUrl ?? ''}:${sourceRevision}`;
		void sourceIdentity;
		shareCopyGeneration += 1;
		shareCopyStatus = '';
		shareCopyFallbackUrl = null;
	});

	/**
	 * Run by the smoke test: fire 10 rapid Range seeks through one
	 * controller (each aborts its predecessor), then one proving seek.
	 * Exposed for `page.evaluate`; also wired to the button for manual use.
	 */
	async function seekStorm(): Promise<{ completed: number; aborted: number; lastBytes: number }> {
		const controller = new SeekController(AUDIO_URL);
		const attempts = Array.from({ length: 10 }, (_, i) =>
			controller.seek(i * 1000, i * 1000 + 99)
		);
		await Promise.allSettled(attempts);
		// The player must never wedge: one more seek works after the storm.
		const after = await controller.seek(0, 99);
		seekReady = true;
		completedSeeks = controller.completed;
		abortedSeeks = controller.aborted;
		lastBytes = after.bytes.length;
		return {
			completed: controller.completed,
			aborted: controller.aborted,
			lastBytes: after.bytes.length
		};
	}

	// A swapped-in element starts at 1x: re-apply the persisted speed.
	$effect(() => {
		audioEl;
		speed?.apply();
	});

	// Smoke-test hook for `page.evaluate`, registered on client mount.
	onMount(() => {
		(window as unknown as { __seekStorm?: typeof seekStorm }).__seekStorm = seekStorm;
		levelingEnabled = loadLevelingEnabled(safeStorage());
		(
			window as unknown as {
				__levelingProbe?: () => {
					enabled: boolean;
					trackId: string | null;
					linear: number;
					contextState: AudioContextState | 'unavailable';
				};
			}
		).__levelingProbe = () => ({
			enabled: levelingEnabled,
			trackId: current?.id ?? null,
			linear: levelingLinear,
			contextState: levelingCtx?.state ?? 'unavailable'
		});
		(window as unknown as { __comforts?: { advanceSleep: typeof advanceSleep } }).__comforts = {
			advanceSleep
		};
		// Reading `localStorage` itself can throw when site storage is
		// blocked (cookie block, sandboxed iframe). Fall back to memory so
		// the rest of setup — queue restore, share links, the tick clock —
		// still runs; speed/resume then work for this page view only.
		let store: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
		try {
			store = localStorage;
		} catch {
			store = memoryStorage();
		}
		speed = createPlaybackSpeed({ storage: store, audio: () => audioEl });
		speedValue = String(speed.current());
		speed.apply();
		resumeStore = createResumeStore({ storage: store });
		try {
			queue = restoreQueue(sessionStorage.getItem(QUEUE_STORAGE_KEY), DEMO_TRACKS);
		} catch {
			// Keep the fixture queue when storage is unavailable.
		}
		// Share links select the track and seek to `t` (plain seconds or
		// clock forms); an unparsable `t` starts at 0, an unknown track
		// keeps the default queue.
		const params = new URLSearchParams(window.location.search);
		const sharedId = params.get('track');
		const shared = DEMO_TRACKS.find((track) => track.id === sharedId);
		if (shared !== undefined) {
			let index = queue.tracks.findIndex((track) => track.id === shared.id);
			if (index < 0) {
				// A restored-but-empty queue (or one missing the track) must
				// not swallow a valid link: re-add the track, then select it.
				queue = addTrack(queue, shared);
				index = queue.tracks.length - 1;
			}
			queue = selectTrack(queue, index);
			const startAt = parseTimestampParam(params.get('t')) ?? 0;
			void tick().then(() => {
				if (current?.id !== shared.id || !audioEl) return;
				try {
					audioEl.currentTime = startAt;
				} catch {
					// The element is not seekable yet; it starts at 0.
				}
			});
		}
		status = current ? 'selected' : 'empty';
		restored = true;
		// An ordinary reload restores the queue but mounts a fresh element
		// at zero: resume the selected track's saved position. Share links
		// already sought above and stay authoritative.
		if (shared === undefined && current) seekToResume(current.id);
		const tickId = setInterval(() => {
			sleepTimer.tick();
		}, 500);
		// A reload never runs the destroy path: persist the live position
		// on hide so resume survives it.
		const persistOnPageHide = () => persistPosition(current?.id);
		window.addEventListener('pagehide', persistOnPageHide);
		// OS media controls are global: release them when the page goes away.
		return () => {
			// A copy settling after teardown must not resurrect state.
			shareCopyAlive = false;
			shareCopyGeneration += 1;
			window.removeEventListener('pagehide', persistOnPageHide);
			clearInterval(tickId);
			clearMediaSession();
			// The leveling graph is page-local: close the context so repeat
			// visits cannot exhaust the browser's context limit, and drop
			// the probe so later visits never read stale state.
			void levelingCtx?.close().catch(() => {});
			levelingCtx = null;
			levelingSource = null;
			levelingGain = null;
			wiredEl = null;
			delete (window as unknown as { __levelingProbe?: unknown }).__levelingProbe;
		};
	});
</script>

<svelte:window onkeydown={handleGlobalKeydown} />

<main class="nt-wrap">
	<h1>Player resilience demo</h1>
	<p data-testid="player-status" aria-live="polite">Status: {status === 'selected' && seekReady ? 'ready' : status}</p>

	<!-- Artwork reuse: the shared Artwork component's placeholder fallback
	     renders for the current track when its art is missing (track 1). -->
	<div data-testid="current-artwork">
		<Artwork src={current?.artworkUrl ?? null} alt={current ? `${current.title} artwork` : 'Track artwork'} />
	</div>
	<!-- Missing R2 art (null) must render the placeholder with alt text. -->
	<div data-testid="null-artwork">
		<Artwork src={null} alt="Demo track artwork" />
	</div>
	<!-- A URL that 404s must swap to the placeholder after the image error. -->
	<div data-testid="broken-artwork">
		<Artwork src="/api/demo/missing-artwork.jpg" alt="Broken demo artwork" />
	</div>

	<div data-testid="fallback-failure-artwork">
		<Artwork
			src="/api/demo/missing-artwork.jpg"
			fallbackSrc="/api/demo/fallback-artwork.jpg"
			alt="Fallback failure artwork"
		/>
	</div>

	<p data-testid="now-playing">
		{#if current}
			{status === 'playing' ? 'Now playing' : 'Selected'}: {current.title} — {current.artist}
		{:else}
			Queue is empty
		{/if}
	</p>

	{#if radio}
		<p data-testid="radio-badge" role="status">Radio</p>
		<button type="button" data-testid="radio-stop" onclick={stopRadio}>
			Stop radio
		</button>
	{/if}

	{#if status === 'error'}
		<div data-testid="player-error" role="alert">
			<p>Could not play this track. Retry playback or choose another track.</p>
			<button type="button" data-testid="player-retry" onclick={() => playCurrent(true)}>
				Retry playback
			</button>
		</div>
	{/if}

	{#key `${current?.streamUrl}:${sourceRevision}`}
		<audio
			bind:this={audioEl}
			use:releaseAudio={current?.id}
			data-testid="demo-audio"
			src={current?.streamUrl}
			preload="metadata"
			controls
			onplay={handlePlay}
			onplaying={handlePlaying}
			onpause={handlePause}
			onerror={handleError}
			onended={handleEnded}
			onvolumechange={handleVolumeChange}
		></audio>
	{/key}

	<div>
		<button type="button" data-testid="player-prev" onclick={userPrevious} disabled={!current}>
			Previous
		</button>
		<button type="button" data-testid="player-next" onclick={userNext} disabled={!current}>
			Next
		</button>
		<LevelingToggle enabled={levelingEnabled} onchange={setLevelingEnabled} />
		<button type="button" data-testid="shortcuts-button" onclick={openShortcuts}>
			Keyboard shortcuts
		</button>
	</div>

	<div>
		<button
			type="button"
			data-testid="share-copy"
			disabled={!shareCopyEnabled}
			onclick={(event) => {
				event.currentTarget.focus();
				void copyShareLink();
			}}
		>
			Copy link at current time
		</button>
		<p data-testid="share-copy-status" aria-live="polite">{shareCopyStatus}</p>
		{#if shareCopyFallbackUrl !== null}
			<div>
				<label for="share-copy-fallback-field">Share link</label>
				<input
					id="share-copy-fallback-field"
					type="text"
					readonly
					data-testid="share-copy-fallback"
					value={shareCopyFallbackUrl}
				/>
				<p data-testid="share-copy-instructions">
					Copy the link manually: focus the field and press Ctrl+C (or Cmd+C on Mac).
				</p>
			</div>
		{/if}
	</div>

	{#if shortcutsOpen}
		<dialog
			bind:this={shortcutsDialogEl}
			data-testid="shortcuts-dialog"
			aria-label="Keyboard shortcuts"
			oncancel={(event) => {
				event.preventDefault();
				closeShortcuts();
			}}
		>
			<h2>Keyboard shortcuts</h2>
			<ul>
				{#each SHORTCUT_ROWS as row (row.key)}
					<li><kbd>{row.key}</kbd> {row.label}</li>
				{/each}
			</ul>
			<button type="button" data-testid="shortcuts-close" onclick={closeShortcuts}>
				Close
			</button>
		</dialog>
	{/if}

	<section data-testid="queue-panel" aria-label="Playback queue">
		<h2>Queue</h2>
		<ol>
			{#each queue.tracks as track, i (track.id)}
				<li data-testid="queue-item" data-track-id={track.id}>
					<button
						type="button"
						data-testid="queue-select-{track.id}"
						aria-current={i === queue.currentIndex ? 'true' : undefined}
						onclick={() => selectAt(i)}
					>
						{track.title} — {track.artist}{i === queue.currentIndex ? (status === 'playing' ? ' (playing)' : ' (selected)') : ''}
					</button>
					<button
						type="button"
						data-testid="queue-move-up-{track.id}"
						aria-label="Move {track.title} up"
						disabled={i === 0}
						onclick={() => (queue = moveTrack(queue, i, i - 1))}
					>
						Up
					</button>
					<button
						type="button"
						data-testid="queue-move-down-{track.id}"
						aria-label="Move {track.title} down"
						disabled={i === queue.tracks.length - 1}
						onclick={() => (queue = moveTrack(queue, i, i + 1))}
					>
						Down
					</button>
				</li>
			{/each}
		</ol>
		<button
			type="button"
			data-testid="queue-clear"
			disabled={queue.tracks.length === 0}
			onclick={clearAll}
		>
			Clear queue
		</button>

		<h3>Add to queue</h3>
		<ul>
			{#each DEMO_TRACKS as track (track.id)}
				<li>
					<button
						type="button"
						data-testid="queue-add-{track.id}"
						disabled={inQueue.has(track.id)}
						onclick={() => addToQueue(track)}
					>
						Add {track.title}
					</button>
				</li>
			{/each}
		</ul>
	</section>

	<section data-testid="comforts-panel" aria-label="Player comforts">
		<h2>Comforts</h2>
		<div>
			<label for="sleep-minutes">Sleep timer (minutes)</label>
			<input
				id="sleep-minutes"
				type="number"
				min="1"
				value="15"
				data-testid="sleep-timer-minutes"
				bind:this={minutesEl}
			/>
			<button type="button" data-testid="sleep-timer-set-duration" onclick={armDuration}>
				Sleep after duration
			</button>
		</div>
		<div>
			<button type="button" data-testid="sleep-timer-set-end-of-track" onclick={armEndOfTrack}>
				Sleep at end of track
			</button>
		</div>
		<div>
			<label for="sleep-tracks">Stop after tracks</label>
			<input
				id="sleep-tracks"
				type="number"
				min="1"
				step="1"
				value="2"
				data-testid="sleep-timer-tracks"
				bind:this={tracksEl}
			/>
			<button type="button" data-testid="sleep-timer-set-stop-after" onclick={armStopAfter}>
				Sleep after N tracks
			</button>
		</div>
		<div>
			<button
				type="button"
				data-testid="sleep-timer-cancel"
				disabled={sleepMode === null}
				onclick={cancelSleep}
			>
				Cancel sleep timer
			</button>
		</div>
		<p data-testid="sleep-timer-status" aria-live="polite">{sleepAnnouncement}</p>
		<div>
			<label for="playback-speed">Playback speed</label>
			<select
				id="playback-speed"
				data-testid="playback-speed"
				value={speedValue}
				onchange={(event) => handleSpeedChange(event.currentTarget.value)}
			>
				<option value="0.5">0.5x</option>
				<option value="0.75">0.75x</option>
				<option value="1">1x</option>
				<option value="1.25">1.25x</option>
				<option value="1.5">1.5x</option>
				<option value="1.75">1.75x</option>
				<option value="2">2x</option>
			</select>
		</div>
	</section>

	<button type="button" data-testid="seek-storm" onclick={() => void seekStorm()}>
		Run 10x seek storm
	</button>
	<p data-testid="seek-stats">completed {completedSeeks}, aborted {abortedSeeks}, last {lastBytes} bytes</p>
</main>
