<script lang="ts">
	import type { RelatedArtist } from '../server/related-artists-shelf.js';

	let { artists }: { artists: readonly RelatedArtist[] } = $props();
	const headingId = $props.id();
</script>

<section aria-labelledby={headingId}>
	<h2 id={headingId}>Related artists</h2>
	{#if artists.length === 0}
		<p>No related artists are available yet.</p>
	{:else}
		<ul>
			{#each artists as artist (artist.id)}
				<li>{artist.name}</li>
			{/each}
		</ul>
	{/if}
</section>
