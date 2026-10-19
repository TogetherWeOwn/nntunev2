<script lang="ts">
	import { currentTrack, type QueueState } from './queue.js';

	interface Props {
		state: QueueState;
		/** Transport stays with the host page: called when Next is activated. */
		onnext?: () => void;
	}

	let { state, onnext }: Props = $props();

	let current = $derived(currentTrack(state));
</script>

<!--
	Reusable queue panel: renders a `QueueState` from the queue store with
	the same hooks the demo player and the queue-order specs rely on.
	The host page owns selection and transport (via `onnext`); the panel
	itself never mutates state.
-->
<p data-testid="now-playing">
	{#if current}
		Now playing: {current.title}{#if current.artist} — {current.artist}{/if}
	{:else}
		Queue is empty
	{/if}
</p>

<button
	type="button"
	data-testid="player-next"
	disabled={!current}
	onclick={() => onnext?.()}
>
	Next
</button>

<ol>
	<!-- Keyed by queue position. Uniqueness comes from the store: addTrack
		dedupes and restoreQueue rejects duplicates. -->
	{#each state.tracks as track, i (i)}
		<li
			data-testid="queue-item"
			data-track-id={track.id}
			aria-current={i === state.currentIndex ? 'true' : undefined}
		>
			{track.title}{#if track.artist} — {track.artist}{/if}
		</li>
	{/each}
</ol>
