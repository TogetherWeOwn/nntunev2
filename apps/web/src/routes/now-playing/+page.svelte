<script lang="ts">
	import { afterNavigate } from '$app/navigation';
	import { page } from '$app/state';
	import { displayArtist, displayTitle } from '#lib/library/order.js';
	import Artwork from '#lib/player/Artwork.svelte';
	import { getAppPlayer } from '#lib/player/AppPlayer.svelte';
	import LyricsPanel, { type TrackLyrics } from '#lib/player/LyricsPanel.svelte';
	import { currentTrack } from '#lib/player/queue.js';

	// No route loader supplies lyrics yet. This is a presentation input only.
	let { lyrics = null }: { lyrics?: TrackLyrics | null } = $props();

	// Presentation only: the layout keeps the existing native controls and decoder.
	const { queue } = getAppPlayer();
	const track = $derived(currentTrack($queue));
	const artist = $derived(track ? displayArtist(track) : null);
	let heading: HTMLHeadingElement | undefined = $state();

	afterNavigate(({ from }) => {
		if (from && from.route.id !== '/now-playing') heading?.focus();
	});
</script>

<svelte:head>
	<title>Now Playing — NNTune</title>
</svelte:head>

<main class="nt-wrap expanded" id="main-content">
	<h1 tabindex="-1" bind:this={heading}>Now Playing</h1>
	{#if page.state.nowPlayingFromMini === true}
		<button type="button" class="nt-button nt-button--ghost back" onclick={() => history.back()}>Back</button>
	{:else}
		<a class="nt-button nt-button--ghost back" href="/library">Your Library</a>
	{/if}

	{#if track}
		<div class="artwork">
			<Artwork />
		</div>
		<h2>{displayTitle(track)}</h2>
		{#if artist}<p class="artist">{artist}</p>{/if}
		<LyricsPanel trackId={track.id} {lyrics} />
	{:else}
		<p>Nothing playing. Choose a track in Your Library.</p>
	{/if}
</main>

<style>
	.expanded {
		width: 100%;
		min-width: 0;
		overflow-wrap: anywhere;
	}
	.back {
		display: inline-flex;
		align-items: center;
		min-height: 3rem;
		font: inherit;
	}
	.artwork {
		max-inline-size: 19.5rem;
		aspect-ratio: 1;
		margin-block-start: var(--nt-space-3);
		overflow: hidden;
		border-radius: var(--nt-radius);
		background: var(--nt-bg-raised);
	}
	.artwork :global(img) {
		display: block;
		inline-size: 100%;
		block-size: 100%;
	}
	.artist {
		color: var(--nt-fg-muted);
	}
</style>
