<script lang="ts">
	import { displayArtist, displayTitle } from '../library/order.js';
	import type { DiscoveryTrack } from '../server/discovery-shelf.js';

	let { tracks }: { tracks: readonly DiscoveryTrack[] } = $props();
	const headingId = $props.id();
</script>

<section aria-labelledby={headingId}>
	<h2 id={headingId}>From the catalogue</h2>
	{#if tracks.length === 0}
		<p>No catalogue tracks are available to discover yet.</p>
	{:else}
		<ul>
			{#each tracks as track (track.id)}
				{@const artist = displayArtist(track)}
				<li>{displayTitle(track)}{artist ? ` — ${artist}` : ''}</li>
			{/each}
		</ul>
	{/if}
</section>
