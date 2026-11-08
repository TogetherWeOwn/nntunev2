<script lang="ts">
	import type { LibraryItem } from './cache.js';
	import { entityLinks } from './entity-links.js';
	import { displayArtist, displayTitle } from './order.js';

	// Sibling of the play button, never inside it: anchors must not nest in
	// the row's button. Renders nothing when the row has no valid ids.
	let { item }: { item: LibraryItem } = $props();
	const links = $derived(entityLinks(item));
</script>

{#if links.albumHref || links.artistHref}
	<span class="nt-library-entities" data-testid="library-entity-links">
		{#if links.albumHref}
			<a href={links.albumHref} data-testid="library-album-link"
				aria-label={`Album for ${displayTitle(item)}`}>Album</a>
		{/if}
		{#if links.artistHref}
			<a href={links.artistHref} data-testid="library-artist-link"
				aria-label={`Artist ${displayArtist(item)}`}>{displayArtist(item)}</a>
		{/if}
	</span>
{/if}
