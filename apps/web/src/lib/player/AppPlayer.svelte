<script module lang="ts">
	import { createContext, onMount, tick, untrack, type Snippet } from 'svelte';
	import { subscribeLocalAuthTransition } from '../auth-transition.js';
	import { browserStorage, LIBRARY_AUTH_TRANSITION_KEY } from '../library/cache.js';
	import { LIBRARY_ACCESS_REFUSED_EVENT } from '../library/refresh.js';
	import { clearMiniPlayerHint } from '../shell/mini-player.js';
	import NativePlayer from './NativePlayer.svelte';
	import PlaybackSpeedControl from './PlaybackSpeedControl.svelte';
	import ShortcutHelp from './ShortcutHelp.svelte';
	import SleepTimerControls from './SleepTimerControls.svelte';
	import VolumeControls from './VolumeControls.svelte';
	import CurrentTrackFavorite from './CurrentTrackFavorite.svelte';
	import { createCurrentTrackFavorite } from './currentTrackFavorite.svelte.js';
	import { appShortcutActions, handleAppShortcut } from './appShortcuts.js';
	import { createPlayerController } from './controller.svelte.js';
	import { createQueue, currentTrack as queueCurrentTrack } from './queue.js';
	export const [getAppPlayer, setAppPlayer] = createContext<ReturnType<typeof createPlayerController> &
		{ claimShareEntry?: () => boolean; currentFavorite: ReturnType<typeof createCurrentTrackFavorite> }>();
</script>
<script lang="ts">
	let { children, claimShareEntry, signedOut, allowDesktopRow, browserPlayback = false }: {
		children: Snippet;
		claimShareEntry?: (owner: string) => boolean;
		signedOut?: boolean;
		allowDesktopRow?: boolean;
		browserPlayback?: boolean;
	} = $props();
	let shareOwner: string | undefined;
	const controller = createPlayerController(createQueue([]), { flush: tick }, undefined, untrack(() => browserPlayback ? {} : undefined));
	const player = setAppPlayer(Object.assign(controller, { currentFavorite: createCurrentTrackFavorite(controller.queue),
		claimShareEntry: () => claimShareEntry?.(shareOwner ??= crypto.randomUUID()) ?? true }));
	const { queue, media, media: { seekFeedback, playbackStatus, absoluteSeek } } = player;
	const { speed, speed: { state: speedState } } = media;
	const attachNativeAudio = media.attach;
	const sound = media.sound.state;
	const currentTrack = $derived(queueCurrentTrack($queue));
	const streamUrl = $derived(player.streamFor(currentTrack));
	const canPrepare = $derived(!!(currentTrack && player.privateListening));
	const sleepTimer = media.sleepTimer;
	let shortcutHelp = $state.raw<ReturnType<typeof ShortcutHelp>>();
	let shortcutScope = $state<HTMLDivElement>();
	function onShortcutKeydown(event: KeyboardEvent) {
		const active = document.activeElement;
		// Require the same player button to own focus and the keydown; native audio keeps its keys.
		if (!streamUrl || !active?.matches('button') || !shortcutScope?.contains(active) || !event.composedPath().includes(active)) return;
		handleAppShortcut(event, media, shortcutHelp);
	}
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
<svelte:window onkeydown={onShortcutKeydown} />
{#snippet soundControls(part: 'controls' | 'status')}
	<VolumeControls {...$sound} {part} onvolumeinput={media.sound.setVolume} ontogglemute={media.sound.toggleMute} />
{/snippet}
<svelte:document onvisibilitychange={() => { if (document.visibilityState === 'visible') sleepTimer.tick(); }} />
{@render children()}
{#snippet favoriteAction(statusOnly: boolean)}<CurrentTrackFavorite currentFavorite={player.currentFavorite} snapshot={$queue} {statusOnly} />{/snippet}
{#if streamUrl || canPrepare}
	<div class="nt-wrap" bind:this={shortcutScope}>
		<NativePlayer {currentTrack} mediaRevision={player.mediaRevision} {streamUrl} {attachNativeAudio} volumeControls={soundControls}
			playbackStatus={streamUrl ? $playbackStatus : 'paused'} playCurrent={media.playCurrent} pauseCurrent={media.pauseCurrent}
			retryCurrent={media.retryCurrent} {allowDesktopRow}
			seekState={$absoluteSeek} seek={media.seek}
			{...$seekFeedback} restartCurrentTrack={media.restart} jumpRelative={media.jump} {favoriteAction} />
		<ShortcutHelp bind:this={shortcutHelp} actions={appShortcutActions} buttonClass="nt-button nt-button--ghost" playerScoped />
	</div>
{/if}
{#if streamUrl}
	<div class="nt-wrap">
		<PlaybackSpeedControl value={$speedState.rate === null ? null : String($speedState.rate)}
			feedback={$speedState.feedback} onchange={(control) => { control.value = String(speed.set(Number(control.value)) ?? ''); }} />
	</div>
	<section class="nt-wrap" aria-label="Sleep timer">
		<SleepTimerControls {...$sleepTimer} start={sleepTimer.start} cancel={sleepTimer.cancel} />
	</section>
{/if}
