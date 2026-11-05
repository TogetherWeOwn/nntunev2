<script lang="ts">
	import { usePlayerSession } from './context.js';
	import { currentTrack } from './queue.js';
	import PlayerQueue from './PlayerQueue.svelte';
	import QueueExport from './QueueExport.svelte';
	import { queueViewActions } from './queueViewActions.js';

	const player = usePlayerSession();
	const actions = queueViewActions(player);
	const favorites = $derived(new Set($player.favorites));
</script>

<div data-testid="queue-panel" data-density={$player.queueDensity}>
	<PlayerQueue
		state={$player.queue}
		selection={$player.queueSelection}
		selectionControls={player}
		showTransport={!currentTrack($player.queue)?.streamUrl}
		showAnnouncement={false}
		favoriteIds={favorites}
		ontogglefavorite={(id) => player.toggleQueuedFavorite(id)}
		bind:filterQuery={() => $player.queueFilterQuery, (query) => player.setQueueFilter(query)}
		liveRegions={$player.queueLiveRegions}
		confirmDestructive
		confirmGeneration={$player.queueConfirmGeneration}
		onnext={actions.onnext}
		onshuffle={actions.onshuffle}
		onmove={actions.onmove}
		onplaynext={actions.onplaynext}
		onremove={actions.onremove}
		onclear={actions.onclear}
		onselect={actions.onselect}
		oncopylink={actions.oncopylink}
		onkeepcurrentandupcoming={actions.onkeepcurrentandupcoming}
		onclearupcoming={actions.onclearupcoming}
	/>
	<QueueExport />
</div>

<style>
	div { --nt-library-row-padding: 0.25rem; }
	div[data-density='compact'] { --nt-library-row-padding: 0; }
	div :global(.queue-row) { padding-block: var(--nt-library-row-padding); }
	div :global(.queue-row button:not(.queue-star)) { min-width: 24px; min-height: 24px; }
</style>
