<script lang="ts">
	import { afterNavigate } from '$app/navigation';
	import { page } from '$app/state';
	import { displayArtist, displayTitle } from '#lib/library/order.js';
	import Artwork from '#lib/player/Artwork.svelte';
	import { getAppPlayer } from '#lib/player/AppPlayer.svelte';
	import LyricsPanel, { type TrackLyrics } from '#lib/player/LyricsPanel.svelte';
	import { verifiedVideoId } from '#lib/player/musicVideo.js';
	import MusicVideoDialog from '#lib/player/MusicVideoDialog.svelte';
	import { currentTrack, type Track } from '#lib/player/queue.js';

	// No route loader supplies lyrics yet. This is a presentation input only.
	let { lyrics = null }: { lyrics?: TrackLyrics | null } = $props();

	// Presentation only: the layout keeps the existing native controls and decoder.
	const { queue, media } = getAppPlayer();
	const track = $derived(currentTrack($queue));
	const artist = $derived(track ? displayArtist(track) : null);
	// Synthetic result from a successful batch check. Only the browser fixture
	// build can admit it. No catalogue/URL candidate is a verified result.
	const fixtureLink = {
		recordingId: 'cfa3bd0c-52a8-4b00-8f28-993d00001601',
		videoId: 'fixture0001',
		verifiedAt: Date.now()
	};
	const videoEnabled = $derived(import.meta.env.MODE === 'browser-fixtures' &&
		page.url.searchParams.get('music-video-fixture') === 'on');
	function getVideoId() {
		return videoEnabled && track ? verifiedVideoId(fixtureLink, track.id) : null;
	}
	let showVideo = $state(false);
	let audioTab: HTMLButtonElement;
	let videoTab: HTMLButtonElement;

	function handleTabKey(event: KeyboardEvent) {
		const tab = event.currentTarget;
		const other = tab === audioTab ? videoTab : audioTab;
		if (event.key === 'ArrowRight' || event.key === 'ArrowLeft') other.focus();
		else if (event.key === 'Home') audioTab.focus();
		else if (event.key === 'End') videoTab.focus();
		else return;
		event.preventDefault();
	}

	$effect(() => {
		if (!getVideoId()) showVideo = false;
	});
	let heading: HTMLHeadingElement | undefined = $state();
	let fullscreenButton: HTMLButtonElement | undefined = $state();
	let fullscreenAvailable = $state(false);
	let fullscreenActive = $state(false);
	let fullscreenPending = $state(false);
	let fullscreenFailure = $state('');
	let toggleFullscreen: (() => Promise<void>) | undefined;

	function observeFullscreen(surface: HTMLElement) {
		const doc = surface.ownerDocument;
		const ownsFullscreen = () => doc.fullscreenElement === surface;
		let disposed = false;
		fullscreenAvailable = doc.fullscreenEnabled === true &&
			typeof surface.requestFullscreen === 'function' && typeof doc.exitFullscreen === 'function';
		if (!fullscreenAvailable) fullscreenFailure = 'Full screen is unavailable in this browser.';

		function syncFullscreen() {
			const active = ownsFullscreen();
			if (active === fullscreenActive) return;
			fullscreenActive = active;
			fullscreenFailure = '';
			if (!surface.isConnected || (!active && doc.fullscreenElement)) return;
			// On exit without a track the control will unmount, even if it is still visible now.
			const control = active || track ? fullscreenButton : undefined;
			const focusTarget = control?.getClientRects().length ? control : heading;
			focusTarget?.focus({ preventScroll: true });
		}

		async function releaseFullscreen() {
			if (!ownsFullscreen()) return;
			try {
				await doc.exitFullscreen();
			} catch {
				// The browser may already be exiting as it removes the surface.
			}
		}

		toggleFullscreen = async () => {
			if (disposed || !surface.isConnected || !fullscreenAvailable || fullscreenPending) return;
			const exiting = ownsFullscreen();
			fullscreenPending = true;
			fullscreenFailure = '';
			try {
				if (exiting) await doc.exitFullscreen();
				else if (!doc.fullscreenElement) await surface.requestFullscreen();
			} catch {
				// A browser-driven change may have won the race. Read the actual owner below.
			}
			if (disposed) {
				// A request that settled after teardown must not retain the surface.
				void releaseFullscreen();
				return;
			}
			if (ownsFullscreen() === exiting) {
				fullscreenFailure = exiting ? 'Could not exit full screen.' : 'Could not enter full screen.';
			}
			fullscreenPending = false;
			syncFullscreen();
		};

		doc.addEventListener('fullscreenchange', syncFullscreen);
		return {
			destroy() {
				disposed = true;
				doc.removeEventListener('fullscreenchange', syncFullscreen);
				void releaseFullscreen();
			}
		};
	}

	afterNavigate(({ from }) => {
		if (from && from.route.id !== '/now-playing') heading?.focus();
	});
</script>

<svelte:head>
	<title>Now Playing — NNTune</title>
</svelte:head>

{#snippet audioContent(item: Track)}
	<div class="artwork">
		<Artwork />
	</div>
	<h2>{displayTitle(item)}</h2>
	{#if artist}<p class="artist">{artist}</p>{/if}
	<LyricsPanel trackId={item.id} {lyrics} />
{/snippet}

<main class="nt-wrap expanded" id="main-content" use:observeFullscreen>
	<h1 tabindex="-1" bind:this={heading}>Now Playing</h1>
	{#if page.state.nowPlayingFromMini === true}
		<button type="button" class="nt-button nt-button--ghost route-button" onclick={() => history.back()}>Back</button>
	{:else}
		<a class="nt-button nt-button--ghost route-button" href="/library">Your Library</a>
	{/if}
	{#if track || fullscreenActive || fullscreenPending}
		<button
			bind:this={fullscreenButton}
			type="button"
			class="nt-button nt-button--ghost route-button fullscreen-control"
			aria-disabled={!fullscreenAvailable || fullscreenPending}
			onclick={() => toggleFullscreen?.()}
		>
			{fullscreenActive ? 'Exit full screen' : 'Full screen'}
		</button>
		<p class="fullscreen-control status" role="status">{fullscreenFailure}</p>
	{/if}

	{#if track}
		{#if getVideoId()}
			<div class="playback-tabs" role="tablist" aria-label="Now Playing views">
				<button type="button" role="tab" id="now-playing-audio-tab" bind:this={audioTab}
					class="nt-button nt-button--ghost" aria-selected={!showVideo} tabindex={showVideo ? -1 : 0}
					onkeydown={handleTabKey} onclick={() => { showVideo = false; }}>Audio</button>
				<button type="button" role="tab" id="now-playing-video-tab" bind:this={videoTab}
					class="nt-button nt-button--ghost" aria-selected={showVideo} tabindex={showVideo ? 0 : -1}
					onkeydown={handleTabKey} onclick={() => { showVideo = true; }}>Music Video</button>
			</div>
			<div role="tabpanel" aria-labelledby="now-playing-audio-tab" hidden={showVideo} tabindex={showVideo ? undefined : 0}>
				{@render audioContent(track)}
			</div>
			<div role="tabpanel" aria-labelledby="now-playing-video-tab" hidden={!showVideo}>
				{#if showVideo}
					<MusicVideoDialog title={displayTitle(track)} {getVideoId}
						pauseAudio={media.pauseCurrent} audioStatus={media.playbackStatus} />
				{/if}
			</div>
		{:else}
			{@render audioContent(track)}
		{/if}
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
	.expanded:fullscreen {
		max-inline-size: none;
		overflow: auto;
		background: var(--nt-bg);
		text-align: center;
	}
	.expanded:fullscreen .artwork {
		inline-size: min(100%, 58vh);
		margin-inline: auto;
	}
	@media (max-width: 799px) {
		.expanded:not(:fullscreen) .fullscreen-control {
			display: none;
		}
	}
	.status {
		margin-block: 0;
	}
	.playback-tabs {
		display: flex;
		overflow-x: auto;
	}
	.playback-tabs button {
		flex: none;
		border-block-end: 2px solid transparent;
	}
	.playback-tabs button[aria-selected='true'] {
		border-block-end-color: currentColor;
	}
	.route-button {
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
