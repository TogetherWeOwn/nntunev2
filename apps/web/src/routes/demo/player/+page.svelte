<svelte:head>
	<title>NNTune — player resilience demo</title>
	<meta name="description" content="Seek-storm and artwork-fallback demo for player smoke tests." />
</svelte:head>

<script lang="ts">
	import { onMount } from 'svelte';
	import Artwork from '#lib/player/Artwork.svelte';
	import { SeekController } from '#lib/player/seekController.js';
	import { applyMediaSession } from '#lib/player/mediaSession.js';
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

	function tryPlay(): void {
		// Synthetic demo bytes may not decode in every engine; a rejected
		// play() must never break queue navigation.
		audioEl?.play().catch(() => {});
	}

	function goNext(): void {
		const advanced = nextTrack(queue);
		if (advanced === queue) {
			status = 'ended';
			return;
		}
		queue = advanced;
		tryPlay();
	}

	function goPrevious(): void {
		queue = previousTrack(queue);
		tryPlay();
	}

	// MediaSession follows the current track; queue order + current track
	// persist for the tab session (sessionStorage, restored on mount).
	$effect(() => {
		applyMediaSession(current, {
			onNext: goNext,
			onPrevious: goPrevious,
			onPlay: tryPlay,
			onPause: () => audioEl?.pause()
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
		onended={goNext}
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
						onclick={() => {
							queue = selectTrack(queue, i);
							tryPlay();
						}}
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
			onclick={() => (queue = clearQueue(queue))}
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
						onclick={() => (queue = addTrack(queue, track))}
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
