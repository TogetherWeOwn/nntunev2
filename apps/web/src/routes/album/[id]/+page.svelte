<script lang="ts">
	import { untrack } from 'svelte';
	import { invalidate } from '$app/navigation';
	import { createCollectionPreference } from '#lib/catalogue/preference.svelte.js';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import Artwork from '#lib/player/Artwork.svelte';
	import ReleaseGrid from '#lib/catalogue/ReleaseGrid.svelte';
	import { catalogueAuthGeneration } from '#lib/catalogue/auth.svelte.js';
	import { discTracks, releaseArtwork, releaseDuration, trackDuration } from '#lib/catalogue/catalogue.js';
	import { releaseOccurrenceLinks } from '#lib/catalogue/occurrence.js';
	import '#lib/catalogue/catalogue.css';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();
	let retrying = $state(false);
	const preference = createCollectionPreference('albums', { baseUrl: PUBLIC_API_BASE_URL });
	$effect(() => {
		const mbid = release?.mbid;
		const generation = data.authGeneration;
		if (mbid) untrack(() => preference.load(mbid, generation));
		else preference.clear();
		return () => preference.clear();
	});
	const authChanged = $derived(data.authGeneration !== catalogueAuthGeneration());
	const detail = $derived(authChanged ? null : data.value);
	const release = $derived(detail?.release);
	const discs = $derived(discTracks(detail?.tracks ?? []));
	const trackLinks = $derived(releaseOccurrenceLinks(detail?.tracks ?? []));
	const duration = $derived(releaseDuration(detail?.tracks ?? []));
	const others = $derived(data.artist?.releases.filter((item) => item.mbid !== release?.mbid) ?? []);
	const versions = $derived(release?.release_group_mbid
		? others.filter((item) => item.release_group_mbid === release?.release_group_mbid) : []);
	const more = $derived(others.filter((item) => !versions.includes(item)));

	async function retry() {
		retrying = true;
		try { await invalidate('catalogue:album'); } finally { retrying = false; }
	}
</script>

<svelte:head><title>{release?.title || 'Album'} · NNTune</title></svelte:head>

<article class="catalogue-page" aria-label="Album">
	{#if release && detail}
		<header class="catalogue-hero">
			<div class="catalogue-cover"><Artwork src={releaseArtwork(release, PUBLIC_API_BASE_URL)} alt={`Cover for ${release.title}`} /></div>
			<div>
				<p>{release.release_type ?? 'Release'} · {release.year ?? 'Year unknown'}</p>
				<h1>{release.title || 'Untitled release'}</h1>
				{#if release.artist_mbid}
					<a href={`/artist/${release.artist_mbid}`}>{release.artist ?? 'Unknown artist'}</a>
				{:else}<p>{release.artist ?? 'Unknown artist'}</p>{/if}
				<p>{detail.tracks.length} available {detail.tracks.length === 1 ? 'track' : 'tracks'} · {duration ?? 'Total duration unknown'}</p>
				<button class="nt-button nt-button--ghost" aria-pressed={preference.value ?? undefined} aria-busy={preference.pending}
					disabled={preference.pending || preference.value === null || preference.failure !== null} onclick={() => preference.toggle()}>
					{preference.value ? 'Saved' : 'Save album'}
				</button>
				{#if preference.pending}
					<p role="status">{preference.value === null ? 'Checking saved status…' : 'Updating saved status…'}</p>
				{:else if preference.failure === 'signed-out'}
					<p role="status">Sign in to manage saved albums.</p><a href="/login">Sign in</a>
				{:else if preference.failure === 'account-deleting'}
					<p role="status">Saved albums cannot change while account deletion is scheduled.</p>
				{:else if preference.failure}
					<p role="status">{preference.failure === 'not-found' ? 'Saved status is unavailable for this release.' : "We couldn't confirm saved status. Check it again before making another change."}</p>
					<button class="nt-button nt-button--ghost" onclick={() => preference.retry()}>Retry saved status</button>
				{/if}
			</div>
		</header>

		<section aria-labelledby="album-tracks">
			<h2 id="album-tracks">Tracks</h2>
			<p class="catalogue-note">Showing recordings available in your library and the shared catalogue.</p>
			{#if detail.tracks.length === 0}
				<p>No recordings from this release are available in your library yet.</p>
			{:else}
				{#each discs as disc (disc.medium)}
					<table class="catalogue-tracks">
						<caption>{discs.length > 1 ? (disc.medium === null ? 'Disc unknown' : `Disc ${disc.medium}`) : 'Available tracks'}</caption>
						<thead><tr><th scope="col">#</th><th scope="col">Title</th><th scope="col">Duration</th></tr></thead>
						<tbody>
							{#each disc.tracks as track, index (`${track.mbid}:${track.track_no}:${index}`)}
								<tr>
									<td>{track.track_no ?? '—'}</td>
									<td>{#if trackLinks.has(track)}<a href={trackLinks.get(track)}>{track.title || 'Untitled track'}</a>{:else}{track.title || 'Untitled track'}{/if}</td>
									<td>{trackDuration(track.duration_ms)}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				{/each}
			{/if}
		</section>
		{#if versions.length}
			<section aria-labelledby="album-versions"><h2 id="album-versions">Other available editions</h2><ReleaseGrid releases={versions} /></section>
		{/if}
		{#if more.length}
			<section aria-labelledby="album-more"><h2 id="album-more">More by {release.artist ?? 'this artist'}</h2><ReleaseGrid releases={more} /></section>
		{/if}
		<section aria-labelledby="album-information">
			<h2 id="album-information">Release information</h2>
			<p>MusicBrainz core metadata (CC0). Release ID: <span class="catalogue-id">{release.mbid}</span></p>
			<p class="catalogue-note">Full credits, label and catalogue details are not available in this mirror response.</p>
		</section>
	{:else}
		<h1>Album</h1>
		{#if authChanged || data.failure === 'signed-out'}
			<p>Sign in to browse releases available to your account.</p><a class="nt-button" href="/login">Sign in</a>
		{:else if data.failure === 'not-found'}
			<p>This release is not available in your catalogue.</p><a href="/library">Go to your library</a>
		{:else}
			<p role="status">We couldn't load this release. Your library has not changed.</p>
			<button class="nt-button" disabled={retrying} onclick={retry}>{retrying ? 'Retrying…' : 'Retry'}</button>
		{/if}
	{/if}
</article>
