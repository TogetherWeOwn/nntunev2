<script lang="ts">
	import { currentTrack, type QueueState } from './queue.js';

	interface Props {
		state: QueueState;
	}

	let { state }: Props = $props();

	let current = $derived(currentTrack(state));
</script>

<!--
	Reusable queue panel: renders a `QueueState` from the queue store with
	the same hooks the demo player and the queue-order specs rely on.
	Presentational only — transport/selection stay with the host page until
	the library→queue wiring follow-up.
-->
<p data-testid="now-playing">
	{#if current}
		Now playing: {current.title} — {current.artist}
	{:else}
		Queue is empty
	{/if}
</p>

<button type="button" data-testid="player-next" disabled={!current}> Next </button>

<ol>
	<!-- Keyed by queue position. Uniqueness comes from the store: addTrack
		dedupes and restoreQueue rejects duplicates. -->
	{#each state.tracks as track, i (i)}
		<li
			data-testid="queue-item"
			data-track-id={track.id}
			aria-current={i === state.currentIndex ? 'true' : undefined}
		>
			{track.title} — {track.artist}
		</li>
	{/each}
</ol>
