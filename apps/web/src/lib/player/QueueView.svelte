<script lang="ts">
	import { usePlayerSession } from './context.js';
	import { currentTrack } from './queue.js';
	import PlayerQueue from './PlayerQueue.svelte';
	import QueueExport from './QueueExport.svelte';
	import { queueViewActions } from './queueViewActions.js';

	const player = usePlayerSession();
	const actions = queueViewActions(player);
	// Transport ticks publish the session, not a new queue interaction lifetime.
	const queue = $derived($player.queue);
	const selection = $derived($player.queueSelection);
	const liveRegions = $derived($player.queueLiveRegions);
	const confirmGeneration = $derived($player.queueConfirmGeneration);
	const filterQuery = $derived($player.queueFilterQuery);
	const favoriteIds = $derived($player.favorites);
	const favorites = $derived(new Set(favoriteIds));
</script>

<div data-testid="queue-panel" data-density={$player.queueDensity}>
	<PlayerQueue
		state={queue}
		selection={selection}
		selectionControls={player}
		interactionsUnavailable={!liveRegions}
		oninsert={actions.oninsert}
		onreorder={actions.onreorder}
		onplaynextids={actions.onplaynextids}
		onremoveids={actions.onremoveids}
		onplaynow={actions.onplaynow}
		onfavoriteids={actions.onfavoriteids}
		showTransport={!currentTrack(queue)?.streamUrl}
		showAnnouncement={false}
		favoriteIds={favorites}
		ontogglefavorite={(id) => player.toggleQueuedFavorite(id)}
		bind:filterQuery={() => filterQuery, (query) => player.setQueueFilter(query)}
		liveRegions={liveRegions}
		confirmDestructive
		confirmGeneration={confirmGeneration}
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
	div :global(.queue-row button:not(.queue-star, .queue-grip, .queue-more)) { min-width: 24px; min-height: 24px; }
</style>
