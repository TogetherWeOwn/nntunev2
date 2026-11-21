<script module lang="ts">
	import { createContext, onMount, tick, untrack, type Snippet } from 'svelte';
	import { subscribeLocalAuthTransition } from '../auth-transition.js';
	import { browserStorage, LIBRARY_AUTH_TRANSITION_KEY } from '../library/cache.js';
	import { LIBRARY_ACCESS_REFUSED_EVENT } from '../library/refresh.js';
	import { resolveStreamUrl } from '../library/stream-url.js';
	import { clearMiniPlayerHint } from '../shell/mini-player.js';
	import NativePlayer from './NativePlayer.svelte';
	import PlaybackSpeedControl from './PlaybackSpeedControl.svelte';
	import SleepTimerControls from './SleepTimerControls.svelte';
	import VolumeControls from './VolumeControls.svelte';
	import { createPlayerController } from './controller.svelte.js';
	import { createQueue, currentTrack as queueCurrentTrack } from './queue.js';
	export const [getAppPlayer, setAppPlayer] = createContext<ReturnType<typeof createPlayerController> & { claimShareEntry?: () => boolean }>();
</script>
<script lang="ts">
	let { children, claimShareEntry, signedOut }: { children: Snippet; claimShareEntry?: (owner: string) => boolean; signedOut?: boolean } = $props();
	let shareOwner: string | undefined;
	const player = setAppPlayer(Object.assign(createPlayerController(createQueue([]), { flush: tick }),
		{ claimShareEntry: () => claimShareEntry?.(shareOwner ??= crypto.randomUUID()) ?? true }));
	const { queue, media, media: { seekFeedback, playbackStatus } } = player;
	const { speed, speed: { state: speedState } } = media;
	const attachNativeAudio = media.attach;
	const sound = media.sound.state;
	const currentTrack = $derived(queueCurrentTrack($queue));
	const streamUrl = $derived(resolveStreamUrl(currentTrack));
	const sleepTimer = media.sleepTimer;
	$effect.pre(() => { if (signedOut) untrack(player.purgeAtAuthBoundary); });
	// Retire the decoder before the mounted Library handles the same signal.
	$effect.pre(() => {
		const offAuth = subscribeLocalAuthTransition(player.purgeAtAuthBoundary);
		const listeners = new AbortController();
		const options = { signal: listeners.signal };
		window.addEventListener('storage', ({ key }) => key === LIBRARY_AUTH_TRANSITION_KEY && player.purgeAtAuthBoundary(), options);
		window.addEventListener(LIBRARY_ACCESS_REFUSED_EVENT, player.purgeAtAuthBoundary, options);
		return () => { offAuth(); listeners.abort(); };
	});
	// Library's pre-effect disposes route state before player teardown.
	onMount(() => {
		const storage = browserStorage('local');
		media.restoreRepeat(storage);
		media.speed.restore(storage);
		return () => { clearMiniPlayerHint(); player.destroy(); };
	});
</script>
{#snippet soundControls()}
	<VolumeControls {...$sound} onvolumeinput={media.sound.setVolume} ontogglemute={media.sound.toggleMute} />
{/snippet}
{#snippet speedControl()}
	<PlaybackSpeedControl value={$speedState.rate === null ? null : String($speedState.rate)}
		feedback={$speedState.feedback} onchange={(control) => {
			control.value = String(speed.set(Number(control.value)) ?? '');
		}} />
{/snippet}
<svelte:document onvisibilitychange={() => { if (document.visibilityState === 'visible') sleepTimer.tick(); }} />
{@render children()}
{#if streamUrl}
	<div class="nt-wrap">
		<NativePlayer {currentTrack} mediaRevision={player.mediaRevision} {streamUrl} {attachNativeAudio} volumeControls={soundControls}
			playbackStatus={$playbackStatus} playCurrent={media.playCurrent} pauseCurrent={media.pauseCurrent}
			retryCurrent={media.retryCurrent}
			{...$seekFeedback} restartCurrentTrack={media.restart} jumpRelative={media.jump} {speedControl} />
	</div>
{/if}
{#if streamUrl}
	<section class="nt-wrap" aria-label="Sleep timer">
		<SleepTimerControls {...$sleepTimer} start={sleepTimer.start} cancel={sleepTimer.cancel} />
	</section>
{/if}
