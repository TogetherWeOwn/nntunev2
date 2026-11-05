<script lang="ts">
	import type { Snippet } from 'svelte';
	import { usePlayerSession } from './context.js';

	let { queueToggle, expanded = false, viewHeader }: {
		queueToggle?: Snippet<[beforeOpen?: () => Promise<boolean>]>;
		expanded?: boolean;
		viewHeader?: Snippet;
	} = $props();
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
		<PlayerTransport {queueToggle} {expanded} />
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
</style>
