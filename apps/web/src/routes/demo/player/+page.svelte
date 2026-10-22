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
	let shortcutsOpen = $state(false);
	let shortcutsOpener: HTMLElement | null = null;

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

	// A source gets its own element: an event queued by the browser before
	// a src swap otherwise has no source/intent identifier we could inspect.
	function releaseAudio(node: HTMLAudioElement) {
		return {
			destroy() {
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

	function selectAt(index: number): void {
		queue = selectTrack(queue, index);
		playCurrent();
	}

	function addToQueue(track: (typeof DEMO_TRACKS)[number]): void {
		const wasEmpty = queue.tracks.length === 0;
		queue = addTrack(queue, track);
		// The first add to an empty queue makes it current: start it like any
		// other track change instead of showing a track that never plays.
		if (wasEmpty) playCurrent();
	}

	function clearAll(): void {
		queue = clearQueue(queue);
		status = 'empty';
		void playback.unload();
	}

	function openShortcuts(): void {
		if (shortcutsOpen) return;
		shortcutsOpener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
		shortcutsOpen = true;
	}

	function closeShortcuts(): void {
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

	function toggleMuteShortcut(): void {
		if (audioEl) audioEl.muted = !audioEl.muted;
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
		goNext();
	}

	// MediaSession follows the current track; queue order + current track
	// persist for the tab session (sessionStorage, restored on mount).
	$effect(() => {
		applyMediaSession(current, {
			onNext: goNext,
			onPrevious: goPrevious,
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

	// Smoke-test hook for `page.evaluate`, registered on client mount.
	onMount(() => {
		(window as unknown as { __seekStorm?: typeof seekStorm }).__seekStorm = seekStorm;
		try {
			queue = restoreQueue(sessionStorage.getItem(QUEUE_STORAGE_KEY), DEMO_TRACKS);
		} catch {
			// Keep the fixture queue when storage is unavailable.
		}
		status = current ? 'selected' : 'empty';
		restored = true;
		// OS media controls are global: release them when the page goes away.
		return () => {
			clearMediaSession();
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
			use:releaseAudio
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
		<button type="button" data-testid="player-prev" onclick={goPrevious} disabled={!current}>
			Previous
		</button>
		<button type="button" data-testid="player-next" onclick={goNext} disabled={!current}>
			Next
		</button>
		<button type="button" data-testid="shortcuts-button" onclick={openShortcuts}>
			Keyboard shortcuts
		</button>
	</div>

	{#if shortcutsOpen}
		<div data-testid="shortcuts-dialog" role="dialog" aria-label="Keyboard shortcuts">
			<h2>Keyboard shortcuts</h2>
			<ul>
				{#each SHORTCUT_ROWS as row (row.key)}
					<li><kbd>{row.key}</kbd> {row.label}</li>
				{/each}
			</ul>
			<button type="button" data-testid="shortcuts-close" onclick={closeShortcuts}>
				Close
			</button>
		</div>
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

	<button type="button" data-testid="seek-storm" onclick={() => void seekStorm()}>
		Run 10x seek storm
	</button>
	<p data-testid="seek-stats">completed {completedSeeks}, aborted {abortedSeeks}, last {lastBytes} bytes</p>
</main>
