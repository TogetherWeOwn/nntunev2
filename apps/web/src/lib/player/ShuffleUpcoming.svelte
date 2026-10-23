<script lang="ts">
	import type { QueueState } from './queue.js';

	interface Props {
		state: QueueState;
		onshuffle?: () => void;
		radioActive?: boolean;
	}

	let { state: queue, onshuffle, radioActive = false }: Props = $props();
	let completed = $state(0);
	const disabled = $derived(
		!onshuffle || radioActive || queue.currentIndex < 0 ||
		queue.tracks.length - queue.currentIndex - 1 < 2
	);

	function shuffle(): void {
		if (disabled) return;
		onshuffle?.();
		completed += 1;
	}
</script>

<button type="button" {disabled} onclick={shuffle}>Shuffle upcoming</button>
<p role="status" aria-live="polite" aria-atomic="true">
	{#if completed > 0}Upcoming tracks shuffled. Shuffle {completed} complete.{/if}
</p>

<style>
	button:focus-visible {
		outline: 2px solid currentColor;
		outline-offset: 3px;
	}
</style>
