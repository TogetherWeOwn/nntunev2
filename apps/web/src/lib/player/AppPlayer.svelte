<script module lang="ts">
	import { createContext, onMount, tick, untrack, type Snippet } from 'svelte';
	import { subscribeLocalAuthTransition } from '../auth-transition.js';
	import { browserStorage, LIBRARY_AUTH_TRANSITION_KEY } from '../library/cache.js';
	import { LIBRARY_ACCESS_REFUSED_EVENT } from '../library/refresh.js';
	import { resolveStreamUrl } from '../library/stream-url.js';
	import { clearMiniPlayerHint } from '../shell/mini-player.js';
	import NativePlayer from './NativePlayer.svelte';
	import ShortcutHelp from './ShortcutHelp.svelte';
	import { appShortcutActions, handleAppShortcut } from './appShortcuts.js';
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
	const attachNativeAudio = media.attach;
	const currentTrack = $derived(queueCurrentTrack($queue));
	const streamUrl = $derived(resolveStreamUrl(currentTrack));
	let shortcutHelp = $state.raw<ReturnType<typeof ShortcutHelp>>();
	let shortcutScope = $state<HTMLDivElement>();
	function onShortcutKeydown(event: KeyboardEvent) {
		if (streamUrl && shortcutScope?.contains(document.activeElement)) handleAppShortcut(event, media, shortcutHelp);
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
		media.restoreRepeat(browserStorage('local'));
		return () => { clearMiniPlayerHint(); player.destroy(); };
	});
</script>
<svelte:window onkeydown={onShortcutKeydown} />
{@render children()}
{#if streamUrl}
	<div class="nt-wrap" bind:this={shortcutScope}>
		<NativePlayer {currentTrack} mediaRevision={player.mediaRevision} {streamUrl} {attachNativeAudio}
			playbackStatus={$playbackStatus} playCurrent={media.playCurrent} pauseCurrent={media.pauseCurrent}
			{...$seekFeedback} restartCurrentTrack={media.restart} jumpRelative={media.jump} />
		<ShortcutHelp bind:this={shortcutHelp} actions={appShortcutActions} buttonClass="nt-button nt-button--ghost" playerScoped />
	</div>
{/if}
