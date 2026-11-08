<script lang="ts">
	import { invalidate } from '$app/navigation';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import Artwork from '#lib/player/Artwork.svelte';
	import { catalogueAuthGeneration } from '#lib/catalogue/auth.svelte.js';
	import { releaseArtwork, trackDuration } from '#lib/catalogue/catalogue.js';
	import '#lib/catalogue/catalogue.css';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();
	let retrying = $state(false);
	const authChanged = $derived(data.authGeneration !== catalogueAuthGeneration());
	const detail = $derived(authChanged ? null : data.value);

	async function retry() {
		retrying = true;
		try { await invalidate('catalogue:track'); } finally { retrying = false; }
	}
</script>

<svelte:head><title>{detail?.track.title || 'Track'} · NNTune</title></svelte:head>

<article class="catalogue-page" aria-label="Track">
	{#if detail}
		<header class="catalogue-hero">
			<div class="catalogue-cover"><Artwork src={releaseArtwork(detail.release, PUBLIC_API_BASE_URL)} alt={`Cover for ${detail.release.title}`} /></div>
			<div>
				<p>Track · {trackDuration(detail.track.duration_ms)}</p>
				<h1>{detail.track.title || 'Untitled track'}</h1>
				{#if detail.track.artist_mbid}
					<a href={`/artist/${detail.track.artist_mbid}`}>{detail.track.artist ?? 'Unknown artist'}</a>
				{:else}<p>{detail.track.artist ?? 'Unknown artist'}</p>{/if}
				<p><a href={`/album/${detail.release.mbid}`}>{detail.release.title || 'Untitled release'}</a></p>
				<p>Disc {detail.track.medium_no ?? 'unknown'} · Track {detail.track.track_no ?? 'unknown'}</p>
			</div>
		</header>
		<p class="catalogue-note">This is a catalogue recording occurrence. A playable source has not been resolved here.</p>
		<section aria-labelledby="track-information">
			<h2 id="track-information">Track information</h2>
			<p>MusicBrainz core metadata (CC0). Recording ID: <span class="catalogue-id">{detail.track.mbid}</span></p>
			<p class="catalogue-note">Full credits are not available in this mirror response.</p>
		</section>
	{:else}
		<h1>Track</h1>
		{#if authChanged || data.failure === 'signed-out'}
			<p>Sign in to browse recordings available to your account.</p><a class="nt-button" href="/login">Sign in</a>
		{:else if data.failure === 'not-found'}
			<p>This recording occurrence is not available in your catalogue.</p><a href="/library">Go to your library</a>
		{:else}
			<p role="status">We couldn't load this recording. Your library has not changed.</p>
			<button class="nt-button" disabled={retrying} onclick={retry}>{retrying ? 'Retrying…' : 'Retry'}</button>
		{/if}
	{/if}
</article>
