<script lang="ts">
	import type { Snippet } from 'svelte';
	import { usePlayerSession } from './context.js';

	let { queueToggle, expanded = false, viewHeader, onNowPlayingOpen }: {
		queueToggle?: Snippet<[beforeOpen?: () => Promise<boolean>]>;
		expanded?: boolean;
		viewHeader?: Snippet;
		onNowPlayingOpen?: (target: HTMLElement, keyboard: boolean) => void;
	} = $props();
	import { displayArtist, displayTitle } from '../library/order.js';
	import { currentTrack } from './queue.js';
	import PlayerTransport from './PlayerTransport.svelte';

	const player = usePlayerSession();
	// Only the current source may load. Upcoming URLs have no non-metering
	// prefetch grant: never mount a second element or warm them with fetch.
	const selected = $derived(currentTrack($player.queue));
	let audio: HTMLAudioElement | undefined = $state();

	$effect(() => {
		if (!audio) return;
		return player.attach(audio);
	});
</script>

{#if selected?.streamUrl}
	{#key `${selected.id}:${selected.streamUrl}`}
		<audio bind:this={audio} data-testid="audio-player" src={selected.streamUrl}></audio>
	{/key}
{/if}

{#snippet controls()}
	{#if selected?.streamUrl}
		<PlayerTransport {queueToggle} {expanded} {onNowPlayingOpen} />
	{:else if selected}
		{#if !expanded}
			<div data-testid="mini-player" class="mini-hint">
				<span class="mini-progress" aria-hidden="true"><span data-testid="mini-player-progress" style:width="0%"></span></span>
				<p>{displayTitle(selected)}{#if displayArtist(selected)}{' — '}{displayArtist(selected)}{/if}</p>
				<button type="button" data-testid="player-play-pause" aria-label="Play" disabled>Play</button>
				{@render queueToggle?.()}
			</div>
		{:else}
			<p>Choose a playable track from Your Library.</p>
			{@render queueToggle?.()}
		{/if}
	{:else}
		{#if expanded}<p>Choose a playable track from Your Library.</p>{/if}
		{@render queueToggle?.()}
	{/if}
{/snippet}

{#if expanded}
	<main id="main-content" data-testid="now-playing-route" data-now-playing-view aria-label="Now playing" tabindex="-1">
		{@render viewHeader?.()}
		{@render controls()}
	</main>
{:else}
	{@render controls()}
{/if}

<style>
	audio { max-width: 100%; }
	.mini-hint { position: relative; display: flex; align-items: center; gap: var(--nt-space-2); }
	.mini-hint p { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
	.mini-progress { display: none; }
	@media (max-width: 799px) {
		.mini-hint { position: sticky; bottom: var(--nt-control); z-index: var(--nt-z-player); height: var(--nt-control-lg); background: var(--nt-bg-raised); border-top: 1px solid var(--nt-border); }
		.mini-progress { position: absolute; inset: 0 0 auto; display: block; height: var(--nt-space-px-2); background: var(--nt-border); pointer-events: none; }
		.mini-progress span { display: block; height: 100%; background: var(--nt-accent); }
	}
</style>
