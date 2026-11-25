<script lang="ts">
	import { tick } from 'svelte';
	import Artwork from '../../../src/lib/player/Artwork.svelte';
	import NativeAudio from '../../../src/lib/player/NativeAudio.svelte';
	import { createPlayback } from '../../../src/lib/player/playback.js';
	import { createQueue, currentTrack, nextTrack, selectTrack } from '../../../src/lib/player/queue.js';
	import { SeekController, type SeekFetch } from '../../../src/lib/player/seekController.js';
	import { ARTWORK_PATH, AUDIO_PATH } from './paths.js';

	const tracks = [
		{ id: 'first', title: 'First Signal', artist: 'Synthetic', streamUrl: AUDIO_PATH },
		{ id: 'second', title: 'Second Signal', artist: 'Synthetic', streamUrl: `${AUDIO_PATH}?track=second`, artworkUrl: ARTWORK_PATH }
	];
	let queue = $state(createQueue(tracks));
	let current = $derived(currentTrack(queue)!);
	let mediaRevision = $state(0);
	let status = $state('selected');
	let seekReady = $state(false);
	let audio: HTMLAudioElement | undefined;

	const playback = createPlayback({
		audio: () => audio,
		flush: tick,
		onPlaybackFailure: () => { status = 'error'; },
		onPlaybackStarted: () => { status = 'playing'; }
	});

	function attachNativeAudio(element: HTMLAudioElement) {
		audio = element;
		const release = playback.observe(element);
		const playing = () => { status = 'playing'; };
		const paused = () => { if (status === 'playing') status = 'paused'; };
		const failed = () => { status = 'error'; };
		element.addEventListener('playing', playing);
		element.addEventListener('pause', paused);
		element.addEventListener('error', failed);
		return () => {
			release();
			element.removeEventListener('playing', playing);
			element.removeEventListener('pause', paused);
			element.removeEventListener('error', failed);
			if (audio === element) audio = undefined;
		};
	}

	function playCurrent() {
		if (status === 'error') mediaRevision += 1;
		status = 'loading';
		void playback.play();
	}

	function createSeekFixture(fetchRange?: SeekFetch) {
		const controller = new SeekController(AUDIO_PATH, fetchRange);
		let position: number | null = null;
		let lastBytes = 0;
		return {
			async seek(start: number, end: number) {
				const result = await controller.seek(start, end);
				position = start;
				lastBytes = result.bytes.length;
				return result;
			},
			snapshot: () => ({
				position, lastBytes, completed: controller.completed, aborted: controller.aborted
			})
		};
	}

	async function seekStorm() {
		const player = createSeekFixture();
		await Promise.allSettled(Array.from({ length: 10 }, (_, i) =>
			player.seek(i * 1000, i * 1000 + 99)
		));
		await player.seek(0, 99);
		seekReady = true;
		const { aborted, completed, lastBytes } = player.snapshot();
		return { aborted, completed, lastBytes };
	}

	// The tests wait for this hook to know the real player mounted.
	Object.assign(window, { seekFixtureStorm: seekStorm, createSeekFixture });
</script>

<p role="status" data-testid="player-status">Status: {status === 'selected' && seekReady ? 'ready' : status}</p>
<p data-testid="now-playing">{current.title}</p>
<div data-testid="current-artwork"><Artwork src={current.artworkUrl} alt={`${current.title} artwork`} /></div>
<div data-testid="null-artwork"><Artwork src={null} alt="Synthetic track artwork" /></div>
<div data-testid="broken-artwork"><Artwork src="/missing-artwork.svg" alt="Broken artwork" /></div>
<div data-testid="fallback-failure-artwork">
	<Artwork src="/missing-artwork.svg" fallbackSrc="/missing-fallback.svg" alt="Fallback failure artwork" />
</div>

{#if status === 'error'}
	<div role="alert" data-testid="player-error">
		<p>Could not play this track. Retry playback or choose another track.</p>
		<button type="button" data-testid="player-retry" onclick={playCurrent}>Retry playback</button>
	</div>
{/if}

<NativeAudio {mediaRevision} streamUrl={current.streamUrl} {attachNativeAudio} />
{#each tracks as track, index (track.id)}
	<button type="button" data-testid={`queue-select-${track.id}`} onclick={() => {
		if (current.id !== track.id) mediaRevision += 1;
		queue = selectTrack(queue, index);
		playCurrent();
	}}>Play {track.title}</button>
{/each}
<button type="button" data-testid="player-next" onclick={() => {
	mediaRevision += 1;
	queue = nextTrack(queue);
}}>Next</button>
