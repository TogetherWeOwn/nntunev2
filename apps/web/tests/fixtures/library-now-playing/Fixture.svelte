<script lang="ts">
	import Layout from '../../../src/routes/+layout.svelte';
	import Library from '../../../src/routes/library/+page.svelte';
	import type { QueueState } from '../../../src/lib/player/queue.js';

	// Engineering Lead authorized a one-time same-PR migration from library-local
	// native controls to the approved persistent custom transport. The bounded
	// executor exception mounts the real layout/child and exports queue input;
	// all other Builder edits remain prohibited. No parallel player is mounted.
	let library: { acceptanceReplaceQueue(next: QueueState): void };
	export function acceptanceReplaceQueue(next: QueueState): void {
		library.acceptanceReplaceQueue(next);
	}
</script>

<Layout>
	<Library bind:this={library} />
</Layout>
