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

	let status = $state('idle');
	let completedSeeks = $state(0);
	let abortedSeeks = $state(0);
	let lastBytes = $state(0);

	let queue: QueueState = $state(createQueue(DEMO_TRACKS));
	let restored = $state(false);
	let audioEl: HTMLAudioElement | undefined = $state();

	const current = $derived(currentTrack(queue));
	const inQueue = $derived(new Set(queue.tracks.map((t) => t.id)));

	const playback = createPlayback({ audio: () => audioEl, flush: tick });

	function resetEnded(): void {
		if (status === 'ended') status = 'idle';
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
		resetEnded();
		void playback.play();
	}

	function goPrevious(): void {
		const previous = previousTrack(queue);
		// Clamped at the first track: nothing changed, so a paused track must
		// not be resumed by a no-op Previous.
		if (previous === queue) return;
		queue = previous;
		resetEnded();
		void playback.play();
	}

	function selectAt(index: number): void {
		queue = selectTrack(queue, index);
		resetEnded();
		void playback.play();
	}

	function addToQueue(track: (typeof DEMO_TRACKS)[number]): void {
		const wasEmpty = queue.tracks.length === 0;
		queue = addTrack(queue, track);
		// The first add to an empty queue makes it current: start it like any
		// other track change instead of showing a track that never plays.
		if (wasEmpty) void playback.play();
	}

	function clearAll(): void {
		queue = clearQueue(queue);
		resetEnded();
		void playback.unload();
	}

	// `ended` is only honoured for the source the queue still points at; a
	// late event from a superseded track must not skip the new one.
	function handleEnded(event: Event): void {
		if (!endedForCurrent(event.currentTarget as HTMLAudioElement, current?.streamUrl)) return;
		goNext();
	}

	// MediaSession follows the current track; queue order + current track
	// persist for the tab session (sessionStorage, restored on mount).
	$effect(() => {
		applyMediaSession(current, {
			onNext: goNext,
			onPrevious: goPrevious,
			onPlay: () => void playback.play(),
			onPause: () => playback.pause()
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
		status = 'ready';
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
		restored = true;
		// OS media controls are global: release them when the page goes away.
		return () => {
			clearMediaSession();
		};
	});
</script>

<main class="nt-wrap">
	<h1>Player resilience demo</h1>
	<p data-testid="player-status">Status: {status}</p>

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
			Now playing: {current.title} — {current.artist}
		{:else}
			Queue is empty
		{/if}
	</p>

	<audio
		bind:this={audioEl}
		data-testid="demo-audio"
		src={current?.streamUrl}
		preload="metadata"
		controls
		onended={handleEnded}
	></audio>

	<div>
		<button type="button" data-testid="player-prev" onclick={goPrevious} disabled={!current}>
			Previous
		</button>
		<button type="button" data-testid="player-next" onclick={goNext} disabled={!current}>
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
						{track.title} — {track.artist}{i === queue.currentIndex ? ' (playing)' : ''}
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
