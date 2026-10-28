<script lang="ts">
	import type { QueueState } from './queue.js';

	interface Props {
		state: QueueState;
		onshuffle?: () => void;
		radioActive?: boolean;
		/**
		 * Whether the completion note is exposed as a live status.
		 * Suspended together with the panel's other statuses while the
		 * host page loads; content is untouched. Defaults to exposed.
		 */
		liveRegions?: boolean;
	}

	let { state: queue, onshuffle, radioActive = false, liveRegions = true }: Props = $props();
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
<p
	role={liveRegions ? 'status' : undefined}
	aria-live={liveRegions ? 'polite' : undefined}
	aria-atomic={liveRegions ? 'true' : undefined}
>
	{#if completed > 0}Upcoming tracks shuffled. Shuffle {completed} complete.{/if}
</p>

<style>
	button:focus-visible {
		outline: 2px solid currentColor;
		outline-offset: 3px;
	}
</style>
