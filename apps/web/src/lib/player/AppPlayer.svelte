<script lang="ts">
	import { onMount, tick, type Snippet } from 'svelte';
	import { subscribeLocalAuthTransition } from '../auth-transition.js';
	import { browserStorage, LIBRARY_AUTH_TRANSITION_KEY } from '../library/cache.js';
	import { resolveStreamUrl } from '../library/stream-url.js';
	import { clearMiniPlayerHint } from '../shell/mini-player.js';
	import { createPlayerController, setAppPlayer } from './controller.svelte.js';
	import NativePlayer from './NativePlayer.svelte';
	import { createQueue, currentTrack as queueCurrentTrack } from './queue.js';

	let { children }: { children: Snippet } = $props();

	const player = setAppPlayer(createPlayerController(createQueue([]), { flush: tick }));
	const { queue, media } = player;
	const seekFeedback = media.seekFeedback;
	const attachNativeAudio = media.attach;
	const currentTrack = $derived(queueCurrentTrack($queue));
	const streamUrl = $derived(resolveStreamUrl(currentTrack));

	// Retire the decoder before the mounted Library handles the same signal.
	$effect.pre(() => {
		const offAuth = subscribeLocalAuthTransition(player.purgeAtAuthBoundary);
		const onStorage = (event: StorageEvent) => {
			if (event.key === LIBRARY_AUTH_TRANSITION_KEY) player.purgeAtAuthBoundary();
		};
		window.addEventListener('storage', onStorage);
		return () => {
			offAuth();
			window.removeEventListener('storage', onStorage);
		};
	});

	onMount(() => {
		media.restoreRepeat(browserStorage('local'));
		return () => {
			clearMiniPlayerHint();
			player.destroy();
		};
	});
</script>

{@render children()}
{#if streamUrl}
	<div class="nt-wrap">
		<NativePlayer
			{currentTrack} mediaRevision={player.mediaRevision} {streamUrl} {attachNativeAudio}
			{...$seekFeedback} restartCurrentTrack={media.restart} jumpRelative={media.jump}
		/>
	</div>
{/if}
