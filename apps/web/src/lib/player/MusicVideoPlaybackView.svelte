<script lang="ts">
	import { page } from '$app/state';
	import type { Snippet } from 'svelte';
	import { displayTitle } from '#lib/library/order.js';
	import { getAppPlayer } from './AppPlayer.svelte';
	import { fixtureLink } from './fixtures/music-video.js';
	import MusicVideoDialog from './MusicVideoDialog.svelte';
	import { verifiedVideoId } from './musicVideo.js';
	import type { Track } from './queue.js';
	let { track, children }: { track: Track; children: Snippet } = $props();
	const { media } = getAppPlayer();
	let showVideo = $state(false);
	let audioTab = $state<HTMLButtonElement>();
	let videoTab = $state<HTMLButtonElement>();

	function getVideoId() {
		if (page.url.searchParams.get('music-video-fixture') !== 'on') return null;
		return verifiedVideoId(fixtureLink, track.id);
	}

	$effect(() => { if (!getVideoId()) showVideo = false; });

	function moveFocus(event: KeyboardEvent) {
		if (event.key !== 'ArrowRight' && event.key !== 'ArrowLeft') return;
		(event.currentTarget === audioTab ? videoTab : audioTab)?.focus();
		event.preventDefault();
	}
</script>

{#if getVideoId()}
	<div class="tabs" role="tablist" aria-label="Now Playing views">
		<button type="button" role="tab" id="now-playing-audio-tab" bind:this={audioTab}
			class="nt-button nt-button--ghost" aria-selected={!showVideo} tabindex={showVideo ? -1 : 0}
			onkeydown={moveFocus} onclick={() => { showVideo = false; }}>Audio</button>
		<button type="button" role="tab" id="now-playing-video-tab" bind:this={videoTab}
			class="nt-button nt-button--ghost" aria-selected={showVideo} tabindex={showVideo ? 0 : -1}
			onkeydown={moveFocus} onclick={() => { showVideo = true; }}>Music Video</button>
	</div>
	<div role="tabpanel" aria-labelledby="now-playing-audio-tab" hidden={showVideo} tabindex={showVideo ? undefined : 0}>
		{@render children()}
	</div>
	<div role="tabpanel" aria-labelledby="now-playing-video-tab" hidden={!showVideo}>
		{#if showVideo}
			{#key track.id}
				<MusicVideoDialog title={displayTitle(track)} {getVideoId}
					pauseAudio={media.pauseCurrent} audioStatus={media.playbackStatus} />
			{/key}
		{/if}
	</div>
{:else}
	{@render children()}
{/if}

<style>
	.tabs { display: flex; overflow-x: auto; }
	.tabs button { flex: none; border-block-end: 2px solid transparent; }
	.tabs button[aria-selected='true'] { border-block-end-color: currentColor; }
</style>
