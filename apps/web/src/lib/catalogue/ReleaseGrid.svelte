<script lang="ts">
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import Artwork from '../player/Artwork.svelte';
	import { groupReleases, releaseArtwork, type Release } from './catalogue.js';

	let { releases }: { releases: Release[] } = $props();
	const groups = $derived(groupReleases(releases));
</script>

<ul class="catalogue-grid">
	{#each groups as editions (editions[0].release_group_mbid ?? editions[0].mbid)}
		<li>
			<a class="catalogue-card" href={`/album/${editions[0].mbid}`}>
				<div class="catalogue-cover"><Artwork src={releaseArtwork(editions[0], PUBLIC_API_BASE_URL)} alt={`Cover for ${editions[0].title}`} /></div>
				<strong>{editions[0].title || 'Untitled release'}</strong>
				<span>{editions[0].year ?? 'Year unknown'} · {editions[0].release_type ?? 'Release'}</span>
			</a>
			{#if editions.length > 1}
				<details>
					<summary>{editions.length} available editions</summary>
					<ul>
						{#each editions as edition (edition.mbid)}
							<li><a href={`/album/${edition.mbid}`}>{edition.title} · {edition.year ?? 'Year unknown'} · {edition.mbid.slice(0, 8)}</a></li>
						{/each}
					</ul>
				</details>
			{/if}
		</li>
	{/each}
</ul>
