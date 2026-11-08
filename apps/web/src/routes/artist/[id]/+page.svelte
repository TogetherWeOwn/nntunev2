<script lang="ts">
	import { invalidate } from '$app/navigation';
	import ReleaseGrid from '#lib/catalogue/ReleaseGrid.svelte';
	import { filterReleases, releaseFilters, type ReleaseFilter } from '#lib/catalogue/catalogue.js';
	import '#lib/catalogue/catalogue.css';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();
	let filter = $state<ReleaseFilter>('all');
	let retrying = $state(false);
	const detail = $derived(data.value);
	const releases = $derived(filterReleases(detail?.releases ?? [], filter));
	$effect(() => { void data.value?.artist.mbid; filter = 'all'; });

	async function retry() {
		retrying = true;
		try { await invalidate('catalogue:artist'); } finally { retrying = false; }
	}
</script>

<svelte:head><title>{detail?.artist.name || 'Artist'} · NNTune</title></svelte:head>

<article class="catalogue-page" aria-label="Artist">
	{#if detail}
		<header class="catalogue-hero">
			<div>
				<p>Artist{detail.artist.type ? ` · ${detail.artist.type}` : ''}</p>
				<h1>{detail.artist.name || 'Unnamed artist'}</h1>
				{#if detail.artist.country}<p>{detail.artist.country}</p>{/if}
			</div>
		</header>
		<section aria-labelledby="artist-popular">
			<h2 id="artist-popular">Popular</h2>
			<p>ListenBrainz popularity is not available here yet. Releases below are not ranked by popularity.</p>
		</section>
		<section aria-labelledby="artist-discography">
			<h2 id="artist-discography">Discography</h2>
			<p class="catalogue-note">Available releases in your library and the shared catalogue, grouped by release group where known.</p>
			<div class="catalogue-filters" role="group" aria-label="Release type">
				{#each releaseFilters as item (item.value)}
					<button class="nt-button nt-button--ghost" aria-pressed={filter === item.value} onclick={() => filter = item.value}>{item.label}</button>
				{/each}
			</div>
			{#if detail.truncated}<p role="status">Showing the first 50 available releases. This discography may be incomplete.</p>{/if}
			{#if releases.length}<ReleaseGrid {releases} />
			{:else}<p role="status">No {filter === 'all' ? 'releases' : releaseFilters.find((item) => item.value === filter)?.label.toLowerCase()} available in this selection.</p>{/if}
		</section>
		<section aria-labelledby="artist-appears-on">
			<h2 id="artist-appears-on">Appears on</h2>
			<p>Guest appearances are not available in this mirror response.</p>
		</section>
		<section aria-labelledby="artist-about">
			<h2 id="artist-about">About this data</h2>
			<p>MusicBrainz core metadata (CC0). Artist ID: <span class="catalogue-id">{detail.artist.mbid}</span></p>
			<p class="catalogue-note">No artist photos, listener estimates or biographies are added.</p>
		</section>
	{:else}
		<h1>Artist</h1>
		{#if data.failure === 'signed-out'}
			<p>Sign in to browse artists available to your account.</p><a class="nt-button" href="/login">Sign in</a>
		{:else if data.failure === 'not-found'}
			<p>This artist is not available in your catalogue.</p><a href="/library">Go to your library</a>
		{:else}
			<p role="status">We couldn't load this artist. Your library has not changed.</p>
			<button class="nt-button" disabled={retrying} onclick={retry}>{retrying ? 'Retrying…' : 'Retry'}</button>
		{/if}
	{/if}
</article>
