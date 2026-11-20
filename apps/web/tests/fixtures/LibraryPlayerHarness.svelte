<script lang="ts">
	import AppPlayer from '../../src/lib/player/AppPlayer.svelte';
	import type { QueueState } from '../../src/lib/player/queue.js';
	import Library from '../../src/routes/library/+page.svelte';

	let library: unknown = $state();
	// The fixtures' Vite pre-transform exposes the existing route setter.
	export const acceptanceReplaceQueue = (state: QueueState) =>
		(library as { acceptanceReplaceQueue(state: QueueState): void }).acceptanceReplaceQueue(state);
</script>
<AppPlayer><Library bind:this={library} /></AppPlayer>
