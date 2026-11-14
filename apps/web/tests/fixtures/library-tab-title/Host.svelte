<script lang="ts">
	import { onMount, tick } from 'svelte';
	import Library from '../../../src/routes/library/+page.svelte';
	import QueueView from '../../../src/lib/player/QueueView.svelte';
	import { createPlayerSession } from '../../../src/lib/player/session.js';
	import { providePlayerSession } from '../../../src/lib/player/context.js';
	import { browserStorage } from '../../../src/lib/library/cache.js';
	import type { QueueState } from '../../../src/lib/player/queue.js';

	// The bounded same-PR exception changes only the fixture's owner wiring.
	// Acceptance expectations and all other fixture protection remain unchanged.
	const player = createPlayerSession({ flush: tick });
	providePlayerSession(player);
	onMount(() => {
		player.start(browserStorage('local'), browserStorage('session'));
		return player.destroy;
	});

	let library: { acceptanceReplaceQueue(next: QueueState): void };
	export function acceptanceReplaceQueue(next: QueueState): void {
		library.acceptanceReplaceQueue(next);
	}
</script>

<Library bind:this={library} />
<QueueView />
