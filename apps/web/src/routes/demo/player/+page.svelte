<svelte:head>
	<title>NNTune — player resilience demo</title>
	<meta name="description" content="Seek-storm and artwork-fallback demo for player smoke tests." />
</svelte:head>

<script lang="ts">
	import { onMount, tick } from 'svelte';
	import Artwork from '#lib/player/Artwork.svelte';
	import { SeekController } from '#lib/player/seekController.js';
	import { applyMediaSession, clearMediaSession } from '#lib/player/mediaSession.js';
	import { createPlayback, endedForCurrent } from '#lib/player/playback.js';
import { createPlaybackSpeed, type PlaybackSpeed } from '#lib/player/playbackSpeed.js';
import { createResumeStore, type ResumeStore } from '#lib/player/resume.js';
import { parseTimestampParam } from '#lib/player/shareTimestamp.js';
import { createSleepTimer, type SleepTimerMode } from '#lib/player/sleepTimer.js';
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

	const current = $derived(currentTrack(queue));
	const inQueue = $derived(new Set(queue.tracks.map((t) => t.id)));

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

	function playCurrent(reload = false): void {
		if (!current) return;
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
		if (status === 'error') playCurrent(true);
		else if (status !== 'playing') status = 'loading';
	}

	function handlePlaying(event: Event): void {
		if (eventForCurrent(event) && (status === 'loading' || status === 'playing')) status = 'playing';
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

	function goPrevious(): void {
		const previous = previousTrack(queue);
		// Clamped at the first track: nothing changed, so a paused track must
		// not be resumed by a no-op Previous.
		if (previous === queue) return;
		queue = previous;
		playCurrent();
	}

	// Explicit navigation persists the outgoing track's position and seeks
	// the incoming track to its saved one. Natural `ended` advances go
	// through goNext directly: the ended track is cleared, not persisted.
	function userNext(): void {
		persistPosition(current?.id);
		goNext();
		seekCurrentResume();
	}

	function userPrevious(): void {
		persistPosition(current?.id);
		goPrevious();
		seekCurrentResume();
	}

	function selectAt(index: number): void {
		const next = selectTrack(queue, index);
		const nextId = next.tracks[next.currentIndex]?.id;
		// Reselecting the current track is not a switch: persisting its
		// untouched position would overwrite a seeded resume entry.
		if (nextId !== undefined && nextId !== current?.id) persistPosition(current?.id);
		queue = next;
		playCurrent();
		if (nextId !== undefined) seekToResume(nextId);
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
		queue = clearQueue(queue);
		status = 'empty';
		void playback.unload();
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
		goNext();
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
		const tickId = setInterval(() => {
			sleepTimer.tick();
		}, 500);
		// A reload never runs the destroy path: persist the live position
		// on hide so resume survives it.
		const persistOnPageHide = () => persistPosition(current?.id);
		window.addEventListener('pagehide', persistOnPageHide);
		// OS media controls are global: release them when the page goes away.
		return () => {
			window.removeEventListener('pagehide', persistOnPageHide);
			clearInterval(tickId);
			clearMediaSession();
		};
	});
</script>

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
		></audio>
	{/key}

	<div>
		<button type="button" data-testid="player-prev" onclick={userPrevious} disabled={!current}>
			Previous
		</button>
		<button type="button" data-testid="player-next" onclick={userNext} disabled={!current}>
			Next
		</button>
	</div>

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
