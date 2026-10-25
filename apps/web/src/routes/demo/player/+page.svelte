<svelte:head>
	<title>NNTune — player resilience demo</title>
	<meta name="description" content="Seek-storm and artwork-fallback demo for player smoke tests." />
</svelte:head>

<script lang="ts">
	import { onMount, tick } from 'svelte';
	import {
		applySoundPreference,
		loadSoundPreference,
		saveSoundPreference,
		type SoundPreference
	} from '#lib/player/soundPreference.js';
	import { page } from '$app/state';
	import { deriveTrackTime } from '#lib/format.js';
	import Artwork from '#lib/player/Artwork.svelte';
	import LevelingToggle from '#lib/player/LevelingToggle.svelte';
	import ShuffleUpcoming from '#lib/player/ShuffleUpcoming.svelte';
	import {
		applyGainToNode,
		loadLevelingEnabled,
		resolveTrackLinearGain,
		saveLevelingEnabled,
		type LevelingTags
	} from '#lib/player/replayGain.js';
	import { SeekController } from '#lib/player/seekController.js';
	import {
		applyMediaSession,
		clearMediaSession,
		setMediaSessionPosition,
		type MediaSessionSeekDetails
	} from '#lib/player/mediaSession.js';
	import { createPlayback, endedForCurrent } from '#lib/player/playback.js';
	import { createRadioOrder, parseRadioSeed } from '#lib/player/radio.js';
	import {
		clampPlaybackSpeed,
		createPlaybackSpeed,
		type PlaybackSpeed
	} from '#lib/player/playbackSpeed.js';
	import { createResumeStore, type ResumeStore } from '#lib/player/resume.js';
	import {
		createRecentlyPlayedStore,
		formatRecentPlayedAt,
		type RecentEntry,
		type RecentlyPlayedStore
	} from '#lib/player/recentlyPlayed.js';
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
		formatQueueText,
		formatQueueM3U,
		moveTrack,
		naturalEndPolicy,
		nextTrack,
		playNext,
		previousTrack,
		restoreQueue,
		selectTrack,
		serializeQueue,
		shuffleUpcoming,
		type QueueState,
		type RepeatMode
	} from '#lib/player/queue.js';
	import { matchesQueueQuery } from '#lib/player/queueFilter.js';

	const AUDIO_URL = '/api/demo/audio';
	const QUEUE_STORAGE_KEY = 'nntune:demo-queue:v1';
	const RADIO_CONTINUE_STORAGE_KEY = 'nntune:radio-continue:v1';

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
	// Buffering describes audibility, not a new transport intent.
	let buffering = $state(false);
	const visibleStatus = $derived(status === 'playing' && buffering ? 'buffering' : status);
	let seekReady = $state(false);
	let sourceRevision = $state(0);
	let completedSeeks = $state(0);
	let abortedSeeks = $state(0);
	let lastBytes = $state(0);

	let queue: QueueState = $state(createQueue(DEMO_TRACKS));
	let restored = $state(false);
	let audioEl: HTMLAudioElement | undefined = $state();
	let sound = $state.raw<SoundPreference & { restricted?: boolean }>({ volume: 1, muted: false });
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
	let radioContinueEnabled = $state(true);
	// Repeat is page-local, unlike the persisted queue: reload always starts Off.
	let repeat = $state<RepeatMode>('off');

	const current = $derived(radio ? (radio.order[radio.index] ?? null) : currentTrack(queue));
	const inQueue = $derived(new Set(queue.tracks.map((t) => t.id)));

	/**
	 * Queue-only filter: transient presentation state, never persisted.
	 * Visible rows keep their ORIGINAL queue indices, so every row action,
	 * disabled boundary and the current marker use the full-queue slot —
	 * filtering hides rows only and never touches order, selection or
	 * playback. Matching is the shared PlayerQueue contract.
	 */
	let queueAnnouncement = $state('');
	let demoFilterQuery = $state('');
	let demoFilterInput: HTMLInputElement | undefined = $state();
	let visibleQueueEntries = $derived(
		queue.tracks
			.map((track, index) => ({ track, index }))
			.filter(({ track }) => matchesQueueQuery(track, demoFilterQuery))
	);

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

	let queueExportStatus = $state('');
	let queueExportFallback = $state<string | null>(null);
	let queueExportGeneration = 0;
	let queueExportAlive = true;
	let queueDownloadUrl: string | null = null;

	function releaseQueueDownload(): void {
		if (queueDownloadUrl !== null) URL.revokeObjectURL(queueDownloadUrl);
		queueDownloadUrl = null;
	}

	// Every queue replacement (including selection) invalidates feedback.
	// Handlers also compare snapshot identity to fence same-tick changes
	// before this effect flushes. Export never writes the queue itself.
	$effect(() => {
		queue;
		queueExportGeneration += 1;
		queueExportStatus = '';
		queueExportFallback = null;
	});

	function beginQueueExport(): number {
		queueExportGeneration += 1;
		queueExportStatus = '';
		queueExportFallback = null;
		return queueExportGeneration;
	}

	async function copyQueueText(): Promise<void> {
		const generation = beginQueueExport();
		const snapshot = queue;
		if (!queueExportAlive || snapshot.tracks.length === 0) return;
		const text = formatQueueText(snapshot);
		try {
			// Both the API getter and the write can fail. Neither is proof
			// of a successful copy: expose selectable text instead.
			if (typeof navigator.clipboard?.writeText !== 'function') throw new Error('No clipboard');
			await navigator.clipboard.writeText(text);
		} catch {
			if (!queueExportAlive || queue !== snapshot || generation !== queueExportGeneration) return;
			queueExportStatus = 'Clipboard unavailable. Copy the queue manually below.';
			queueExportFallback = text;
			return;
		}
		if (!queueExportAlive || queue !== snapshot || generation !== queueExportGeneration) return;
		queueExportStatus = 'Copied queue as text.';
	}

	function downloadQueueM3U(): void {
		beginQueueExport();
		if (!queueExportAlive || queue.tracks.length === 0) return;
		const link = document.createElement('a');
		try {
			// Retain at most one Blob URL until the next download or teardown,
			// not an arbitrary timer while the browser may still be saving it.
			releaseQueueDownload();
			const blob = new Blob([formatQueueM3U(queue)], { type: 'audio/x-mpegurl;charset=utf-8' });
			queueDownloadUrl = URL.createObjectURL(blob);
			link.href = queueDownloadUrl;
			link.download = 'queue.m3u';
			link.hidden = true;
			document.body.append(link);
			link.click();
			// The browser owns the save dialog: initiating it cannot prove
			// that a file was saved to disk.
			queueExportStatus = 'Download started. Check your browser’s saved downloads for queue.m3u.';
		} catch {
			releaseQueueDownload();
			queueExportStatus = 'Could not start the download. Try again or copy the queue as text.';
		} finally {
			link.remove();
		}
	}

	const playback = createPlayback({
		audio: () => audioEl,
		flush: tick,
		onPlaybackFailure: () => {
			status = 'error';
		},
		onPlaybackStarted: () => {
			status = 'playing';
		},
		onBufferingChange: (value) => {
			buffering = value;
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

	function loadRadioContinueEnabled(): boolean {
		try {
			const raw = safeStorage()?.getItem(RADIO_CONTINUE_STORAGE_KEY);
			if (raw === null || raw === undefined) return true;
			const parsed: unknown = JSON.parse(raw);
			return typeof parsed === 'boolean' ? parsed : true;
		} catch {
			return true;
		}
	}

	function setRadioContinueEnabled(enabled: boolean): void {
		// This changes only future queue exhaustion, never an active radio pass.
		radioContinueEnabled = enabled;
		try {
			safeStorage()?.setItem(RADIO_CONTINUE_STORAGE_KEY, JSON.stringify(enabled));
		} catch {
			// Storage failures leave the in-session preference usable.
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
	/** Concise failure note when a requested rate does not apply; empty on success. */
	let speedFeedback = $state('');
	// Retain a mismatch through the ratechange emitted by the same media write.
	let speedFeedbackRate: number | null = null;
	const PRESET_SPEED_VALUES = ['0.5', '0.75', '1', '1.25', '1.5', '1.75', '2'];
	/** A restored or native non-preset rate renders as its own explicit option. */
	const showCustomSpeed = $derived(!PRESET_SPEED_VALUES.includes(speedValue));
	let resumeStore: ResumeStore | null = null;

	// Session-local recently-played list (demo synthetic tracks only; the
	// store itself is fail-soft, so a corrupt key renders the empty state).
	let recentStore: RecentlyPlayedStore | null = null;
	let recentEntries = $state<RecentEntry[]>([]);
	// Polite action-only feedback for history Add/Forget: empty until the
	// first activation, never set on load, play, seek or timeupdate.
	let recentFeedback = $state('');
	let recentSection: HTMLElement | undefined = $state();

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

	function handleSpeedChange(control: HTMLSelectElement): void {
		const value = control.value;
		if (!speed) {
			speedValue = value;
			return;
		}
		// The store verifies against the element and returns the actual
		// applied rate: the selection names that rate, never the request.
		const effective = speed.set(Number(value));
		speedValue = String(effective);
		// A rejected request leaves state unchanged, so one-way
		// `value={speedValue}` would keep showing the refused choice: revert
		// the control itself to the verified rate.
		control.value = String(effective);
		if (effective !== clampPlaybackSpeed(Number(value))) {
			speedFeedback = `Speed ${value}x not applied — still ${effective}x`;
			speedFeedbackRate = effective;
		} else {
			speedFeedback = '';
			speedFeedbackRate = null;
		}
	}

	/**
	 * Reconcile a native-control rate change to state, display and
	 * best-effort persistence. Reads only: never writes back into the
	 * element (no feedback loop) and never implies play or seek. Detached,
	 * stale, throwing or non-finite readings are ignored so they cannot
	 * poison the last verified preference.
	 */
	function handleNativeRateChange(event: Event): void {
		const element = event.currentTarget as HTMLAudioElement | null;
		if (!element || element !== audioEl || !element.isConnected) return;
		if (!speed) return;
		let nativeRate: number;
		try {
			nativeRate = element.playbackRate;
		} catch {
			return;
		}
		if (!Number.isFinite(nativeRate)) return;
		const effective = speed.adoptNativeRate(nativeRate);
		speedValue = String(effective);
		if (speedFeedbackRate !== null && effective !== speedFeedbackRate) {
			speedFeedback = '';
			speedFeedbackRate = null;
		}
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

	interface RecentRow extends RecentEntry {
		title: string;
		artist: string;
		/** False for stale ids outside the demo catalogue: shown as unavailable, never resolved to another track. */
		available: boolean;
	}

	/**
	 * Rendered rows: entries resolved against the demo catalogue. Unknown ids
	 * stay stored (the cap counts them) and render an honest unavailable row
	 * with no playback or queue actions, so a seeded or stale key can never
	 * show a non-demo title.
	 */
	function toRecentRows(entries: RecentEntry[]): RecentRow[] {
		return entries.map((entry) => {
			const track = DEMO_TRACKS.find((candidate) => candidate.id === entry.trackId);
			return track === undefined
				? { ...entry, title: 'Unavailable track', artist: '', available: false }
				: { ...entry, title: track.title, artist: track.artist, available: true };
		});
	}

	const recentTracks = $derived(toRecentRows(recentEntries));

	function refreshRecent(): void {
		recentEntries = recentStore?.entries() ?? [];
	}

	/** Record an explicit or completed demo play (no-op before mount). */
	function recordRecent(trackId: string | undefined): void {
		if (!trackId || !recentStore) return;
		recentStore.record(trackId);
		refreshRecent();
	}

	function recentLabel(playedAt: number): string {
		return formatRecentPlayedAt(playedAt, Date.now());
	}

	function clearRecent(): void {
		recentStore?.clear();
		refreshRecent();
	}

	function replayRecent(trackId: string): void {
		// A cleared queue may no longer hold the track: re-add the demo
		// catalogue entry, then select it like any explicit pick (which also
		// moves it to the front of the recent list without duplicating).
		const deck = DEMO_TRACKS.find((track) => track.id === trackId);
		if (!deck) return;
		let index = queue.tracks.findIndex((track) => track.id === trackId);
		if (index < 0) {
			queue = addTrack(queue, deck);
			index = queue.tracks.length - 1;
		}
		selectAt(index);
	}

	/**
	 * Append a history entry to the queue without touching playback: no
	 * play, seek, source change or restart, and no history rewrite. Uses
	 * the shared addTrack semantics (append only if missing), so a repeat
	 * activation is an explained no-op. Unknown ids never resolve to
	 * another track.
	 */
	function enqueueRecent(trackId: string): void {
		const deck = DEMO_TRACKS.find((track) => track.id === trackId);
		if (!deck) {
			recentFeedback = 'That history entry is unavailable.';
			return;
		}
		if (queue.tracks.some((track) => track.id === trackId)) {
			recentFeedback = `${deck.title} is already in the queue.`;
			return;
		}
		queue = addTrack(queue, deck);
		if (status === 'empty') status = 'selected';
		recentFeedback = `Added ${deck.title} to the queue.`;
	}

	/** Forget one history entry: queue, current media and resume are untouched. */
	function forgetRecent(row: RecentRow, removedIndex: number): void {
		if (!recentStore) return;
		recentStore.forget(row.trackId);
		refreshRecent();
		recentFeedback = row.available
			? `Forgot ${row.title} from recent history.`
			: 'Forgot the unavailable entry from recent history.';
		void focusAfterForget(removedIndex);
	}

	/**
	 * Keep keyboard focus on an enabled surviving history control after a
	 * row is removed: the row taking the removed slot, else the last row's
	 * first action, else Clear when the list is empty.
	 */
	async function focusAfterForget(removedIndex: number): Promise<void> {
		await tick();
		const section = recentSection;
		if (!section) return;
		const rows = section.querySelectorAll('li[data-testid="recent-item"]');
		const targetRow = rows[Math.min(removedIndex, rows.length - 1)];
		const target =
			targetRow?.querySelector('button:not([disabled])') ??
			section.querySelector('[data-testid="recently-played-clear"]');
		(target as HTMLElement | null)?.focus?.();
	}

	// A source gets its own element: an event queued by the browser before
	// a src swap otherwise has no source/intent identifier we could inspect.
	function releaseAudio(node: HTMLAudioElement, trackId: string | undefined) {
		if (restored) updateSound(sound, node, false);
		const stopObserving = playback.observe(node);
		return {
			destroy() {
				stopObserving();
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

	// Current-track time display: a page-session elapsed/remaining choice plus
	// coordinates read only from the current matching audio element. No
	// persistence, no transport calls, no live region for per-second updates.
	let timeMode = $state<'elapsed' | 'remaining'>('elapsed');
	let trackTime = $state(0);
	let trackDuration = $state(NaN);

	// Time events are honoured only from the mounted element whose current
	// resource is still the queue's track. A connected element reporting a
	// different currentSrc (or a detached one) cannot supply coordinates.
	function timeEventForCurrent(element: HTMLAudioElement): boolean {
		if (element !== audioEl || !element.isConnected) return false;
		const expected = current?.streamUrl;
		if (!expected) return false;
		try {
			return element.currentSrc === new URL(expected, document.baseURI).href;
		} catch {
			return false;
		}
	}

	function handleTrackTime(event: Event): void {
		// Bound directly to the audio element, currentTarget is always that
		// element during dispatch; the identity check below fences the rest.
		const target = event.currentTarget as HTMLAudioElement;
		if (!timeEventForCurrent(target)) return;
		trackTime = target.currentTime;
		trackDuration = target.duration;
	}

	// Any source replacement (track change, retry revision, queue clear or
	// re-add) resets the coordinates; the mode choice survives within the page.
	$effect(() => {
		current?.streamUrl;
		sourceRevision;
		trackTime = 0;
		trackDuration = NaN;
	});

	const trackTimeSummary = $derived.by(() => {
		if (!current) return 'No track selected';
		const derived = deriveTrackTime(trackTime, trackDuration);
		// Both modes share one total fragment: a known duration reads
		// "of <total>", an unknown one names the missing coordinate.
		const total = derived.duration === null ? '— Duration unavailable' : `of ${derived.duration}`;
		if (timeMode !== 'remaining') return `Elapsed ${derived.elapsed} ${total}`;
		if (derived.remaining === null) return `Remaining unavailable ${total}`;
		return `Remaining ${derived.remaining} ${total}`;
	});

	function handlePlay(event: Event): void {
		// A queued play event can arrive after a newer pause. The element's
		// current paused flag distinguishes that from an explicit native resume.
		if (!eventForCurrent(event) || (event.currentTarget as HTMLAudioElement).paused) return;
		// Native controls bypass playCurrent: retry the resume on the gesture.
		resumeLevelingContext();
		if (status === 'error') playCurrent(true);
		else if (status !== 'playing') {
			// A play event outside a playCurrent flight is a native-control
			// press: record it like any explicit play-press. playCurrent sets
			// status 'loading' synchronously before the async play, so its own
			// plays (and retries) always arrive as 'loading' and record at
			// their own call sites — or not at all.
			if (status !== 'loading') recordRecent(current?.id);
			status = 'loading';
		}
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
	// the incoming track to its saved one. Natural completion is separate:
	// the ended track is cleared, not persisted.
	function userNext(): void {
		const outgoingId = current?.id;
		persistPosition(outgoingId);
		goNext();
		if (current?.id !== outgoingId) {
			seekCurrentResume();
			// Explicit Next records the newly playing track. Auto-advances go
			// through goNext directly and record nothing extra.
			recordRecent(current?.id);
		}
	}

	function userPrevious(): void {
		const outgoingId = current?.id;
		persistPosition(outgoingId);
		goPrevious();
		if (current?.id !== outgoingId) {
			seekCurrentResume();
			recordRecent(current?.id);
		}
	}

	/** Clearing restores every row and keeps focus inside the panel. */
	function clearDemoFilter(): void {
		demoFilterQuery = '';
		demoFilterInput?.focus();
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
		// An explicit pick records even when its playback fails; only the
		// retry afterwards must not append.
		recordRecent(nextId);
		if (nextId !== undefined && shouldResume) seekToResume(nextId);
	}

	async function playNextAt(index: number, control: HTMLButtonElement): Promise<void> {
		// Radio owns what plays next (radio.order), so reordering the resting
		// queue would announce a move the button cannot fulfil. ShuffleUpcoming
		// disables itself the same way; the button mirrors it below.
		if (radio) return;
		const track = queue.tracks[index];
		const reordered = playNext(queue, index);
		// Current/already-next requests leave even the previous announcement alone.
		if (reordered === queue || !track) return;
		queue = reordered;
		queueAnnouncement = `${track.title} moved to position ${reordered.currentIndex + 2} of ${reordered.tracks.length}`;
		// A keyed row can move in the DOM without keeping browser focus.
		await tick();
		control.focus({ preventScroll: true });
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

	// Explicit Tab wrap for the shortcuts dialog. Native showModal keeps the
	// background inert, but Chromium does not cycle Tab past the dialog's
	// last/first control: focus falls back to body instead of wrapping,
	// which KEYSAFE-B11 pins as a failure (same workaround as PaywallModal
	// wrapTab, which documents the stutter). Cycling the rendered controls
	// here restores the documented trap in every browser. Scoped to this
	// dialog only: page tab order and modified chords pass through, and an
	// already-handled Tab keeps its prior ownership.
	function trapShortcutsTab(event: KeyboardEvent): void {
		if (event.key !== 'Tab') return;
		if (event.defaultPrevented) return;
		if (event.metaKey || event.ctrlKey || event.altKey) return;
		const root = shortcutsDialogEl;
		if (!root) return;
		const controls = [...root.querySelectorAll<HTMLElement>(
			'a[href], button:not([disabled]), input, select, textarea, [tabindex]:not([tabindex="-1"])'
		)].filter((control) => control.getClientRects().length > 0);
		if (controls.length === 0) {
			event.preventDefault();
			return;
		}
		const first = controls[0];
		const last = controls[controls.length - 1];
		const active = document.activeElement;
		if (event.shiftKey) {
			if (active === first || !root.contains(active)) {
				event.preventDefault();
				last.focus();
			}
		} else if (active === last || !root.contains(active)) {
			event.preventDefault();
			first.focus();
		}
	}

	function togglePlayShortcut(): void {
		if (status === 'playing' || status === 'loading') pauseCurrent();
		else {
			// Snapshot before playCurrent changes status to loading: an
			// error-state press retries without recording another play.
			const retry = status === 'error';
			playCurrent(retry);
			if (!retry) recordRecent(current?.id);
		}
	}

	function seekShortcut(deltaSeconds: number): void {
		const el = audioEl;
		if (!el) return;
		const max = Number.isFinite(el.duration) ? el.duration : Number.POSITIVE_INFINITY;
		el.currentTime = Math.min(Math.max(0, el.currentTime + deltaSeconds), max);
	}

	// Mount and keyed replacements apply before playback's tick resolves.
	// Native events use the same equality-guarded path: no setter feedback.
	function updateSound(preference: SoundPreference, element = audioEl, persist = true): void {
		if (!element) return;
		const actual = applySoundPreference(element, preference);
		actual.restricted ||= sound.restricted ?? false;
		if (persist) {
			if (sound.volume !== actual.volume || sound.muted !== actual.muted) {
				saveSoundPreference(safeStorage(), actual);
			}
		}
		sound = actual;
	}

	// Usable-media gate shared by the jump, restart and keyboard seeks:
	// the current element is read at activation and unusable media (no
	// current track, stale source, non-finite or negative time, non-finite
	// or non-positive duration) refuses quietly instead of throwing through
	// the UI or reporting a seek that never happened.
	function seekCoordinates(): { element: HTMLAudioElement } | null {
		const el = audioEl;
		if (!el || !current) return null;
		try {
			// Same-task navigation changes the queue before the keyed element
			// swaps: a src mismatch is the previous track, never seek it.
			if (el.getAttribute('src') !== current.streamUrl) return null;
		} catch {
			return null;
		}
		let position = Number.NaN;
		let bound = Number.NaN;
		try {
			position = el.currentTime;
			bound = el.duration;
		} catch {
			// An unreadable element is unusable media: refuse below.
		}
		if (
			!Number.isFinite(position) ||
			position < 0 ||
			!Number.isFinite(bound) ||
			bound <= 0
		)
			return null;
		return { element: el };
	}

	// Visible jump controls share the keyboard relative-seek path above:
	// SEEK_SECONDS stays the single step source and the gate above fences
	// unusable media before the write.
	function seekButton(deltaSeconds: number): void {
		if (!seekCoordinates()) return;
		try {
			seekShortcut(deltaSeconds);
		} catch {
			// A non-seekable element refuses the assignment; keep the
			// previous position instead of throwing through the UI.
		}
	}

	// Restart seeks the current element to zero through the same validated
	// gate: it reads the live element at activation (never a cached one, so
	// a source swap between renders cannot mistarget), preserves
	// paused/playing intent and rate by touching nothing but currentTime,
	// and never advances tracks, calls play, or records accounting.
	function restartCurrent(): void {
		const coords = seekCoordinates();
		if (!coords) return;
		try {
			coords.element.currentTime = 0;
		} catch {
			// A non-seekable element refuses the assignment; keep the
			// previous position instead of throwing through the UI.
		}
	}

	// Explicit "Go to time" seek: the form reuses the shared timestamp
	// parser (seconds, m:ss, h:mm:ss — the same grammar as inbound share
	// links), then independently guards finite range, live duration and
	// the current matching element before writing currentTime. Every
	// refusal explains itself inline; nothing clamps, autoplays or moves
	// focus into a modal.
	const GOTO_HINT_ID = 'player-goto-hint';
	const GOTO_ERROR_ID = 'player-goto-error-text';
	let gotoValue = $state('');
	let gotoError = $state('');
	let gotoStatus = $state('');

	function submitGotoTime(): void {
		// Each submission replaces prior feedback: a refusal clears an
		// older success and a success clears an older refusal.
		gotoError = '';
		gotoStatus = '';
		const seconds = parseTimestampParam(gotoValue);
		if (
			seconds === null ||
			!Number.isFinite(seconds) ||
			seconds < 0 ||
			seconds > Number.MAX_SAFE_INTEGER
		) {
			gotoError = 'Enter a valid time in seconds, m:ss, or h:mm:ss.';
			return;
		}
		const el = audioEl;
		if (!el || !current) {
			gotoError = 'No track is selected, so there is no media to seek. Add or select a track first.';
			return;
		}
		// The bound element must still be the live one for this track: a
		// detached node or a source swap since render is unavailable media,
		// never a stale reference to write through.
		if (!el.isConnected || !endedForCurrent(el, current.streamUrl)) {
			gotoError = 'Track media is not available right now. Wait for it to load and try again.';
			return;
		}
		// Duration is read from the live element at submission, never from
		// a cached coordinate: it can shrink or go unknown after editing.
		let duration = Number.NaN;
		try {
			duration = el.duration;
		} catch {
			duration = Number.NaN;
		}
		if (!Number.isFinite(duration) || duration < 0) {
			gotoError = 'Duration is unavailable — the track has not loaded enough to seek yet.';
			return;
		}
		// An explicit out-of-range request is corrected, never clamped.
		if (seconds > duration) {
			gotoError = 'That time is past the end of this track. Enter a time within the duration.';
			return;
		}
		try {
			el.currentTime = seconds;
			// An exact-end seek during active or pending playback must become an
			// intentional pause before native completion can advance the queue.
			if (seconds >= duration && (status === 'playing' || status === 'loading')) pauseCurrent();
		} catch {
			// The setter refused (a non-seekable resource): report honestly
			// and never echo decoder internals into the page.
			gotoError = 'Could not seek this track. Try again once it is playing.';
			return;
		}
		gotoStatus = `Seeked to ${seconds} seconds.`;
	}


	function handleVolumeChange(event: Event & { currentTarget: HTMLAudioElement }): void {
		if (restored && eventForCurrent(event)) updateSound(event.currentTarget);
	}

	function handleVolumeInput(event: Event & { currentTarget: HTMLInputElement }): void {
		if (!audioEl || !restored) return;
		updateSound({ volume: Number(event.currentTarget.value) / 100, muted: audioEl.muted });
		// A rejected setter leaves state unchanged: undo the range's own edit.
		event.currentTarget.value = String(sound.volume * 100);
	}

	function toggleMuteShortcut(): void {
		if (!audioEl || !restored) return;
		updateSound({ volume: audioEl.volume, muted: !audioEl.muted });
	}

	// Global transport keymap. Escape closes our dialog first so a modal
	// guard can never wedge it; every other binding yields to typing
	// surfaces, open modals and modified chords. Cancelled or composing keys
	// keep native ownership: no preventDefault and no player mutation.
	// Handled bindings suppress their keydown default: the digit binding
	// would otherwise also drive native select type-ahead (the 0.5x/0.75x
	// speed options start with 0), so one keystroke would restart the track
	// AND halve the speed.
	function handleGlobalKeydown(event: KeyboardEvent): void {
		if (event.metaKey || event.ctrlKey || event.altKey) return;
		// Prior ownership wins: a cancelled event was already handled, and an
		// isComposing or legacy 229 key belongs to the IME, not the player.
		if (event.defaultPrevented || event.isComposing || event.keyCode === 229) return;
		const action = actionForKey(event.key);
		if (!action) return;
		if (action === 'close-shortcuts') {
			if (shortcutsOpen) closeShortcuts();
			return;
		}
		if (shortcutsOpen || isTypingTarget(event.target) || isModalOpen(document)) return;
		event.preventDefault();
		switch (action) {
			case 'toggle-play':
				togglePlayShortcut();
				break;
			case 'next-track':
				// Same gesture as the Next button: persist, navigate, resume
				// and record through the shared explicit-play path.
				userNext();
				break;
			case 'previous-track':
				userPrevious();
				break;
			case 'seek-back':
				// Same validated/clamped gate as the visible jump controls.
				seekButton(-SEEK_SECONDS);
				break;
			case 'seek-forward':
				seekButton(SEEK_SECONDS);
				break;
			case 'restart-track':
				restartCurrent();
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
		// Off preserves the existing completion path after a resource error;
		// repeat requires an active transport, never an errored resource.
		const playing = status === 'playing' || status === 'loading' ||
			(repeat === 'off' && status === 'error');
		if (!eventForCurrent(event) || !playing || !current) return;
		const endedId = current.id;
		// The completed track's entry covers the listen (queue and radio
		// alike); the auto-advanced track earns its own on completion.
		recordRecent(endedId);
		const fired = sleepTimer.trackEnded();
		// A finished track never resumes mid-way, whether the timer fired or
		// the queue advances.
		resumeStore?.clear(endedId);
		const result = naturalEndPolicy(queue, {
			repeat,
			radioActive: radio !== null,
			// The preference applies only after the queue ends with repeat Off.
			continueRadio: radioContinueEnabled,
			playing,
			validEnd: true,
			sleepComplete: fired
		});
		if (result.action === 'stop') {
			// A fired timer already paused via onFire; keep that paused state.
			if (!fired) {
				playback.pause();
				status = 'ended';
			}
			return;
		}
		if (result.action === 'ignore') return;
		if (result.action === 'radio') {
			if (radio) advanceRadio();
			else startRadio();
			seekCurrentResume();
			return;
		}
		queue = result.queue;
		// A fresh keyed element starts even a singleton replay at zero and
		// fences late events from the completed resource. Never use audio.loop:
		// each completion must still reach sleep timers and resume clearing.
		playCurrent(repeat !== 'off');
		if (repeat === 'off') seekCurrentResume();
	}

	// Scope OS callbacks and timeline events to this exact source/element.
	// Cleanup fences retained callbacks as well as removing event listeners.
	$effect(() => {
		const track = current;
		const el = audioEl;
		const revision = sourceRevision;
		let active = true;
		const isCurrent = () =>
			active && current === track && audioEl === el && sourceRevision === revision &&
			!!el && endedForCurrent(el, track?.streamUrl);
		const publishPosition = () => {
			if (!isCurrent() || !el) return;
			setMediaSessionPosition({
				duration: el.duration,
				position: el.currentTime,
				playbackRate: el.playbackRate
			});
		};
		const seek = (details: MediaSessionSeekDetails, direction: -1 | 0 | 1) => {
			if (!isCurrent() || !el) return;
			const duration = el.duration;
			if (!Number.isFinite(duration) || duration <= 0) return;
			let target: number;
			if (direction === 0) {
				const time = details?.seekTime;
				if (typeof time !== 'number' || !Number.isFinite(time) || time < 0) return;
				target = time;
			} else {
				const offset = details?.seekOffset === undefined ? 10 : details.seekOffset;
				if (
					typeof offset !== 'number' || !Number.isFinite(offset) || offset <= 0 ||
					!Number.isFinite(el.currentTime)
				) return;
				target = el.currentTime + direction * offset;
			}
			try {
				el.currentTime = Math.min(duration, Math.max(0, target));
				// Seeking to the end must not synthesize natural completion and
				// advance the queue/start radio, including while play() is pending.
				if (target >= duration) pauseCurrent();
			} catch {
				// Some resources reject a seek despite having loaded metadata.
				return;
			}
			publishPosition();
		};
		applyMediaSession(track, {
			onNext: () => {
				if (isCurrent()) userNext();
			},
			onPrevious: () => {
				if (isCurrent()) userPrevious();
			},
			onPlay: () => {
				if (!isCurrent()) return;
				const retry = status === 'error';
				playCurrent(retry);
				if (!retry) recordRecent(current?.id);
			},
			onPause: () => {
				if (isCurrent()) pauseCurrent();
			},
			onSeekTo: (details) => seek(details, 0),
			onSeekForward: (details) => seek(details, 1),
			onSeekBackward: (details) => seek(details, -1)
		});
		const events = ['loadedmetadata', 'durationchange', 'timeupdate', 'seeked', 'ratechange'];
		for (const event of events) el?.addEventListener(event, publishPosition);
		// Clear first on a track change; metadata may already be available if
		// the source came from cache, otherwise its events repopulate state.
		publishPosition();
		return () => {
			active = false;
			for (const event of events) el?.removeEventListener(event, publishPosition);
		};
	});

	// Queue order + current track persist for the tab session.
	$effect(() => {
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

	// The same switch clears explicit-seek feedback, so a stale success or
	// correction never lingers past a track change. The entered value stays:
	// only the outcome belongs to the previous source.
	$effect(() => {
		const gotoIdentity = `${current?.id ?? ''}:${current?.streamUrl ?? ''}:${sourceRevision}`;
		void gotoIdentity;
		gotoError = '';
		gotoStatus = '';
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
	// `apply` verifies against the element, so reconcile the display to
	// the actual rate it reports (a rejecting/clamping element must not
	// leave a false selection on screen).
	$effect(() => {
		audioEl;
		if (speed) speedValue = String(speed.apply());
	});

	// Smoke-test hook for `page.evaluate`, registered on client mount.
	onMount(() => {
		sound = loadSoundPreference(safeStorage());
		updateSound(sound, audioEl, false);
		(window as unknown as { __seekStorm?: typeof seekStorm }).__seekStorm = seekStorm;
		levelingEnabled = loadLevelingEnabled(safeStorage());
		radioContinueEnabled = loadRadioContinueEnabled();
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
		speedValue = String(speed.apply());
		resumeStore = createResumeStore({ storage: store });
		try {
			recentStore = createRecentlyPlayedStore({ storage: sessionStorage });
		} catch {
			recentStore = createRecentlyPlayedStore({ storage: memoryStorage() });
		}
		refreshRecent();
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
			queueExportAlive = false;
			queueExportGeneration += 1;
			releaseQueueDownload();
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
	<p data-testid="player-status" role="status" aria-live="polite" aria-atomic="true">Status: {visibleStatus === 'selected' && seekReady ? 'ready' : visibleStatus}</p>

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
			{visibleStatus === 'playing' ? 'Now playing' : 'Selected'}: {current.title} — {current.artist}
		{:else}
			Queue is empty
		{/if}
	</p>

	<label>
		<input
			type="checkbox"
			checked={radioContinueEnabled}
			aria-describedby="radio-continue-scope"
			onchange={(event) => setRadioContinueEnabled(event.currentTarget.checked)}
		/>
		Continue with radio when queue ends
	</label>
	<p id="radio-continue-scope">
		Demo tracks only, not live library playback. This choice applies when the queue ends with Repeat Off;
		it does not stop a track or radio already playing.
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
			ontimeupdate={handleTrackTime}
			onloadedmetadata={handleTrackTime}
			ondurationchange={handleTrackTime}
			onratechange={handleNativeRateChange}
		></audio>
	{/key}

	<div class="nt-sound">
		<label>
			Volume {Math.round(sound.volume * 100)}%
			<input
				type="range"
				min="0"
				max="100"
				value={sound.volume * 100}
				disabled={!restored || sound.restricted}
				oninput={handleVolumeInput}
			/>
		</label>
		<button type="button" aria-pressed={sound.muted} onclick={toggleMuteShortcut}>
			Mute
		</button>
	</div>
	<p role="status" hidden={!sound.restricted}>Use system volume controls.</p>

	<div>
		<label for="track-time-mode">Time display</label>
		<select id="track-time-mode" data-testid="track-time-mode" bind:value={timeMode}>
			<option value="elapsed">Elapsed</option>
			<option value="remaining">Remaining</option>
		</select>
	</div>
	<p data-testid="track-time-summary">{trackTimeSummary}</p>

	<div>
		<button type="button" data-testid="player-prev" onclick={userPrevious} disabled={!current}>
			Previous
		</button>
		<button type="button" data-testid="player-next" onclick={userNext} disabled={!current}>
			Next
		</button>
		<label for="player-repeat">Repeat</label>
		<select
			id="player-repeat"
			data-testid="player-repeat"
			bind:value={repeat}
			disabled={radio !== null}
			aria-describedby={radio ? 'repeat-radio-explanation' : undefined}
		>
			<option value="off">Off</option>
			<option value="one">One</option>
			<option value="queue">Queue</option>
		</select>
		<p data-testid="repeat-status" role="status">Repeat: {repeat === 'one' ? 'One' : repeat === 'queue' ? 'Queue' : 'Off'}</p>
		{#if radio}
			<p id="repeat-radio-explanation">Repeat is unavailable while radio is active. Stop radio to change repeat.</p>
		{/if}
		<!-- Own row so the pair stays side by side at 360px instead of
		     wrapping around Previous/Next into overlapping lines. -->
		<div>
			<button
				type="button"
				data-testid="player-seek-back"
				onclick={() => seekButton(-SEEK_SECONDS)}
				disabled={!current}
			>
				Back 10 seconds
			</button>
			<button
				type="button"
				data-testid="player-seek-forward"
				onclick={() => seekButton(SEEK_SECONDS)}
				disabled={!current}
			>
				Forward 10 seconds
			</button>
		</div>
		<!-- Own row like the jump pair above: three transport buttons in one
		     row would overflow the 360px viewport. -->
		<div>
			<button type="button" data-testid="player-restart" onclick={restartCurrent} disabled={!current}>
				Restart track
			</button>
		</div>
		<LevelingToggle enabled={levelingEnabled} onchange={setLevelingEnabled} />
		<button type="button" data-testid="shortcuts-button" onclick={openShortcuts}>
			Keyboard shortcuts
		</button>
	</div>

	<!-- Explicit seek for the synthetic demo track: a native form so Enter
	     submits it and typing never reaches the global transport keymap. -->
	<div>
		<form
			class="goto-form"
			onsubmit={(event) => {
				event.preventDefault();
				submitGotoTime();
			}}
		>
			<label for="player-goto-input">Go to time</label>
			<input
				id="player-goto-input"
				type="text"
				name="goto-time"
				autocomplete="off"
				spellcheck="false"
				data-testid="player-goto-time"
				bind:value={gotoValue}
				aria-describedby={gotoError ? `${GOTO_HINT_ID} ${GOTO_ERROR_ID}` : GOTO_HINT_ID}
				aria-invalid={gotoError ? 'true' : undefined}
			/>
			<p id={GOTO_HINT_ID}>Enter seconds, m:ss, or h:mm:ss.</p>
			<button type="submit" data-testid="player-goto-seek">Seek</button>
			{#if gotoError}
				<p data-testid="player-goto-error" id={GOTO_ERROR_ID} role="alert">{gotoError}</p>
			{/if}
			<p data-testid="player-goto-status" role="status">{gotoStatus}</p>
		</form>
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
			onkeydown={trapShortcutsTab}
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
		<p data-testid="queue-announcement" role="status">{queueAnnouncement}</p>
		<div class="queue-export">
			<div class="queue-export-actions">
				<button
					type="button"
					data-testid="queue-export-copy"
					disabled={queue.tracks.length === 0}
					aria-describedby="queue-export-scope queue-export-status"
					onclick={(event) => {
						event.currentTarget.focus();
						void copyQueueText();
					}}
				>
					Copy queue as text
				</button>
				<button
					type="button"
					data-testid="queue-export-download"
					disabled={queue.tracks.length === 0}
					aria-describedby="queue-export-scope queue-export-status"
					onclick={downloadQueueM3U}
				>
					Download queue.m3u
				</button>
			</div>
			<p id="queue-export-scope">Whole queue, in arranged order. Track IDs only in M3U — no audio or stream links.</p>
			<p id="queue-export-status" data-testid="queue-export-status" aria-live="polite">
				{queue.tracks.length === 0 ? 'Nothing to export: the track list is empty. Add a track first.' : queueExportStatus}
			</p>
			{#if queueExportFallback !== null}
				<label for="queue-export-fallback-field">Queue as text</label>
				<textarea
					id="queue-export-fallback-field"
					data-testid="queue-export-fallback"
					readonly
					rows="4"
					value={queueExportFallback}
					aria-describedby="queue-export-instructions"
				></textarea>
				<p id="queue-export-instructions" data-testid="queue-export-instructions">
					Copy manually: focus the field, select all (Ctrl+A or Cmd+A), then copy (Ctrl+C or Cmd+C).
					On touch screens, long-press the text, select all and choose Copy.
				</p>
			{/if}
		</div>
		<ShuffleUpcoming
			state={queue}
			radioActive={radio !== null}
			onshuffle={() => {
				if (!radio) queue = shuffleUpcoming(queue);
			}}
		/>
		<!--
			Queue-only filter: transient presentation state. It hides rows but
			never reorders, reselects, persists or fetches anything.
		-->
		<div class="queue-filter">
			<label for="demo-queue-filter-input">Filter queue</label>
			<input
				id="demo-queue-filter-input"
				type="search"
				bind:value={demoFilterQuery}
				bind:this={demoFilterInput}
			/>
			<button type="button" onclick={clearDemoFilter}>Clear queue filter</button>
		</div>
		<p role="status">{visibleQueueEntries.length} of {queue.tracks.length} tracks</p>
		{#if queue.tracks.length > 0 && visibleQueueEntries.length === 0}
			<p role="status">No matches in this queue</p>
		{/if}
		<ol>
			{#each visibleQueueEntries as { track, index } (track.id)}
				<li data-testid="queue-item" data-track-id={track.id}>
					<button
						type="button"
						data-testid="queue-select-{track.id}"
						aria-current={index === queue.currentIndex ? 'true' : undefined}
						onclick={() => selectAt(index)}
					>
						{track.title} — {track.artist}{index === queue.currentIndex ? (visibleStatus === 'playing' ? ' (playing)' : ' (selected)') : ''}
					</button>
					<button
						type="button"
						data-testid="queue-move-up-{track.id}"
						aria-label="Move {track.title} up"
						disabled={index === 0}
						onclick={() => (queue = moveTrack(queue, index, index - 1))}
					>
						Up
					</button>
					<button
						type="button"
						data-testid="queue-move-down-{track.id}"
						aria-label="Move {track.title} down"
						disabled={index === queue.tracks.length - 1}
						onclick={() => (queue = moveTrack(queue, index, index + 1))}
					>
						Down
					</button>
					<button
						type="button"
						class="queue-play-next"
						data-testid="queue-play-next-{track.id}"
						aria-label="Play {track.title} next"
						disabled={radio !== null}
						onclick={(event) => void playNextAt(index, event.currentTarget)}
					>
						Play next
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

	<section data-testid="recently-played" aria-label="Recently played" bind:this={recentSection}>
		<h2>Recently played</h2>
		<p>
			Demo session history — synthetic demo tracks only. Clearing the queue never wipes this
			list; only Clear below does.
		</p>
		<p role="status">{recentFeedback}</p>
		{#if recentTracks.length === 0}
			<p data-testid="recently-played-empty">
				Nothing played yet this demo session — press play to start your recent list.
			</p>
		{:else}
			<ol>
				{#each recentTracks as row, index (row.trackId)}
					<li class="recent-row" data-testid="recent-item" data-track-id={row.trackId}>
						{row.title}{#if row.artist} — {row.artist}{/if}
						<span data-testid="recent-played-at-{row.trackId}">{recentLabel(row.playedAt)}</span>
						{#if row.available}
							<button
								type="button"
								data-testid="recent-replay-{row.trackId}"
								onclick={() => replayRecent(row.trackId)}
							>
								Replay {row.title}
							</button>
							<button
								type="button"
								aria-label={`Add ${row.title} to queue`}
								onclick={() => enqueueRecent(row.trackId)}
							>
								Add to queue
							</button>
						{/if}
						<button
							type="button"
							aria-label={row.available
								? `Forget ${row.title} from recent history`
								: `Forget unavailable entry ${row.trackId} from recent history`}
							onclick={() => forgetRecent(row, index)}
						>
							Forget this entry
						</button>
					</li>
				{/each}
			</ol>
		{/if}
		<button type="button" data-testid="recently-played-clear" onclick={clearRecent}>
			Clear recently played
		</button>
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
				onchange={(event) => handleSpeedChange(event.currentTarget)}
			>
				{#if showCustomSpeed}
					<option value={speedValue}>{speedValue}x</option>
				{/if}
				<option value="0.5">0.5x</option>
				<option value="0.75">0.75x</option>
				<option value="1">1x</option>
				<option value="1.25">1.25x</option>
				<option value="1.5">1.5x</option>
				<option value="1.75">1.75x</option>
				<option value="2">2x</option>
			</select>
			{#if speedFeedback}
				<p data-testid="speed-status" role="status">{speedFeedback}</p>
			{/if}
		</div>
	</section>

	<button type="button" data-testid="seek-storm" onclick={() => void seekStorm()}>
		Run 10x seek storm
	</button>
	<p data-testid="seek-stats">completed {completedSeeks}, aborted {abortedSeeks}, last {lastBytes} bytes</p>
</main>

<style>
	.queue-export {
		margin-block: var(--nt-space-3);
	}

	.queue-export-actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--nt-space-2);
	}

	.queue-export button {
		min-height: 2.75rem;
		max-width: 100%;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
	}

	.queue-export p {
		margin-block: var(--nt-space-2);
	}

	#queue-export-scope {
		color: var(--nt-fg-muted);
	}

	.queue-export textarea {
		display: block;
		width: 100%;
		min-width: 0;
		max-width: 100%;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}

	/* The mode choice keeps a visible focus ring under programmatic focus
	   too, where the global :focus-visible rule does not apply. */
	#track-time-mode:focus {
		outline: 2px solid var(--nt-accent, currentColor);
		outline-offset: 2px;
	}

	.queue-play-next {
		min-height: 2.75rem;
		min-width: 2.75rem;
		max-width: 100%;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
	}

	/* Queue-only filter row: wraps on narrow screens, touch-sized
	   controls, same contract as the library PlayerQueue panel. */
	.queue-filter {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--nt-space-2);
	}

	.queue-filter input {
		flex: 1 1 12rem;
		min-width: 0;
		max-width: 100%;
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}

	.queue-filter input:focus-visible {
		outline: 2px solid var(--nt-accent);
		outline-offset: 2px;
	}

	.queue-filter button {
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
	}

	/* Explicit-seek form: same wrap/shrink contract as the queue filter
	   row so the entry and Seek stay inside the 360px viewport. */
	.goto-form {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--nt-space-2);
	}

	.goto-form input {
		flex: 1 1 12rem;
		min-width: 0;
		max-width: 100%;
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}

	.goto-form input:focus-visible {
		outline: 2px solid var(--nt-accent);
		outline-offset: 2px;
	}

	.goto-form button {
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
	}

	/* Recently-played row actions: a reachable touch floor on narrow
	   screens (the row text still wraps, so nothing widens past the
	   viewport). */
	.recent-row button {
		min-height: 2.75rem;
		min-width: 2.75rem;
	}
</style>
