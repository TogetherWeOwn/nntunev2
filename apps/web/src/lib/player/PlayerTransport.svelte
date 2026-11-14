<script lang="ts">
	import { tick, type Snippet } from 'svelte';

	let { queueToggle, expanded = false, onNowPlayingOpen }: { queueToggle?: Snippet<[beforeOpen?: () => Promise<boolean>]>; expanded?: boolean; onNowPlayingOpen?: (target: HTMLElement, keyboard: boolean) => void } = $props();
	import IconSvg from '../design/IconSvg.svelte';
	import { FAVORITE_ICON_PATHS, TRANSPORT_ICON_PATHS } from '../design/icons.js';
	import { formatDuration } from '../format.js';
	import { displayArtist, displayTitle } from '../library/order.js';
	import { usePlayerSession } from './context.js';
	import { currentTrack } from './queue.js';
	import { isModalOpen, isTypingTarget } from './shortcuts.js';
	import { actionForPlayerKey, PLAYER_SHORTCUT_ROWS } from './appShortcuts.js';
	import Artwork from './Artwork.svelte';
	import { createFullscreenController, type FullscreenSnapshot } from './fullscreen.js';

	const player = usePlayerSession();
	const transport = $derived($player.transport);
	const active = $derived(transport?.status === 'playing' || transport?.status === 'loading' || transport?.status === 'buffering');
	const duration = $derived(transport?.duration ?? 0);
	const position = $derived(Math.min(duration, transport?.position ?? 0));
	const volume = $derived(Math.round((transport?.sound.volume ?? 1) * 100));
	const sleep = $derived(transport?.sleep);
	const sleepText = $derived(!sleep ? 'Sleep timer off' : sleep.kind === 'duration'
		? `Sleep in ${Math.ceil(sleep.remainingMs / 60000)} minutes`
		: sleep.kind === 'end-of-track' ? 'Sleep at end of track'
		: `Sleep after ${sleep.remainingTracks} tracks`);
	// The compact bar replaces the full-controls restart below 800px.
	// Only one `library-restart` node is mounted at a time; match the CSS
	// breakpoint so the visible copy switches with the layout.
	let compact = $state(false);
	$effect(() => {
		const query = window.matchMedia('(max-width: 799px)');
		const sync = () => { compact = query.matches; };
		sync();
		query.addEventListener('change', sync);
		return () => query.removeEventListener('change', sync);
	});

	let feedback = $state('');
	let dialog: HTMLDialogElement | undefined = $state();
	let opener: HTMLElement | null = null;
	let helpOpen = $state(false);
	const id = $props.id();
	const selected = $derived(currentTrack($player.queue));
	const favorite = $derived(!!selected && $player.favorites.includes(selected.id));
	const progress = $derived(duration > 0 ? Math.min(100, (position / duration) * 100) : 0);
	let fullscreen = $state<FullscreenSnapshot>({ supported: false, active: false, pending: false, failed: false });
	let fullscreenController: ReturnType<typeof createFullscreenController> | undefined;

	function mountFullscreen(element: HTMLElement) {
		fullscreenController = createFullscreenController(element, (state) => { fullscreen = state; });
		return { destroy: () => { fullscreenController?.destroy(); fullscreenController = undefined; } };
	}

	function noteOpen(event: MouseEvent): void {
		// Plain anchor navigation: modifier/middle clicks keep native behavior,
		// and the layout only records the opener for focus restoration.
		if (event.defaultPrevented || event.button !== 0 || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
		onNowPlayingOpen?.(event.currentTarget as HTMLElement, event.detail === 0);
	}

	$effect(() => {
		// A failed seek belongs to its source, not to the next selection.
		selected?.id;
		selected?.streamUrl;
		feedback = '';
	});

	function seek(seconds: number): void {
		feedback = player.seek(seconds) ? '' : 'Could not seek this track. Try again once it has loaded.';
	}

	async function openHelp(): Promise<void> {
		if (helpOpen) return;
		opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
		helpOpen = true;
		await tick();
		if (helpOpen) dialog?.showModal();
	}

	function closeHelp(): void {
		dialog?.close();
		helpOpen = false;
		if (opener?.isConnected) opener.focus();
		opener = null;
	}

	function onDialogKey(event: KeyboardEvent): void {
		if (event.key !== 'Tab' || event.altKey || event.ctrlKey || event.metaKey || !dialog) return;
		const buttons = [...dialog.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')];
		const first = buttons[0];
		const last = buttons[buttons.length - 1];
		if (event.shiftKey && document.activeElement === first) {
			event.preventDefault(); last?.focus();
		} else if (!event.shiftKey && document.activeElement === last) {
			event.preventDefault(); first?.focus();
		}
	}

	function onKey(event: KeyboardEvent): void {
		if (event.defaultPrevented || event.repeat || event.isComposing || isTypingTarget(event.target) || isModalOpen(document) ||
			(event.target instanceof Element && event.target.closest('[role="slider"]'))) return;
		if (event.altKey && !event.ctrlKey && !event.metaKey && (event.key === 'ArrowUp' || event.key === 'ArrowDown')) {
			event.preventDefault();
			player.setVolume((transport?.sound.volume ?? 1) + (event.key === 'ArrowUp' ? 0.05 : -0.05));
			return;
		}
		if (event.altKey || event.ctrlKey || event.metaKey) return;
		if (event.key === 'Escape' && fullscreen.active) {
			event.preventDefault();
			void fullscreenController?.exit();
			return;
		}
		// Space still activates a focused native button/link; only passive
		// player/main surfaces get the global transport equivalent.
		const target = event.target instanceof HTMLElement ? event.target : null;
		const space = event.key === ' ' && target?.closest('main, [data-player-transport]') &&
			!target.closest('button, a, summary, [role="button"], [role="slider"]');
		const action = space ? 'toggle-play' : actionForPlayerKey(event.key);
		if (!action || action === 'close-shortcuts') return;
		event.preventDefault();
		switch (action) {
			case 'toggle-play': player.togglePlay(); break;
			case 'next-track': void player.next(); break;
			case 'previous-track': void player.previous(); break;
			case 'seek-back': seek((transport?.position ?? 0) - (event.shiftKey ? 30 : 10)); break;
			case 'seek-forward': seek((transport?.position ?? 0) + (event.shiftKey ? 30 : 10)); break;
			case 'restart-track': player.restart(); break;
			case 'toggle-mute': player.toggleMute(); break;
			case 'shuffle-upcoming': player.shuffle(); break;
			case 'cycle-repeat': player.cycleRepeat(); break;
			case 'toggle-favorite': player.toggleCurrentFavorite(); break;
			case 'open-shortcuts': void openHelp(); break;
		}
	}

	function onSleepChange(event: Event): void {
		const value = (event.currentTarget as HTMLSelectElement).value;
		if (value === 'off') player.cancelSleep();
		else if (value === 'end') player.armSleep({ kind: 'end-of-track' });
		else player.armSleep({ kind: 'duration', ms: Number(value) * 60000 });
	}
</script>

<svelte:window onkeydown={onKey} />

{#snippet favoriteControl()}
	<button type="button" data-testid="player-favorite" aria-label={favorite ? 'Remove from Favorites' : 'Save to Favorites'} title={favorite ? 'Remove from Favorites' : 'Save to Favorites'} aria-pressed={favorite} onclick={() => player.toggleCurrentFavorite()}>
		<IconSvg path={FAVORITE_ICON_PATHS[favorite ? 'heart-filled' : 'heart']} />
	</button>
{/snippet}

{#snippet playControl()}
	<button type="button" class="play" data-testid="player-play-pause" aria-label={active ? 'Pause' : 'Play'} title={active ? 'Pause' : 'Play'} disabled={!selected} onclick={() => player.togglePlay()}>
		<IconSvg path={TRANSPORT_ICON_PATHS[active ? 'pause' : 'play']} />
	</button>
{/snippet}

{#snippet seekControl()}
	<div class="seek compact-hide">
		<span>{formatDuration(position)}</span>
		<input type="range" data-testid="player-seek" aria-label="Seek" aria-valuetext={`${formatDuration(position)} of ${duration ? formatDuration(duration) : 'unknown duration'}`} min="0" max={duration || 1} step="0.1" value={position} disabled={!duration} oninput={(event) => seek(Number(event.currentTarget.value))} />
		<span>{duration ? formatDuration(duration) : '—'}</span>
	</div>
	<div class="transport-buttons compact-hide">
		<button type="button" data-testid="player-seek-back" onclick={() => player.jumpRelative('back')}>Back 10 seconds</button>
		<button type="button" data-testid="player-seek-forward" onclick={() => player.jumpRelative('forward')}>Forward 10 seconds</button>
	</div>
	<p data-testid="player-seek-feedback" aria-live="polite">{#key $player.jumpStatusVersion}{$player.jumpStatus}{/key}</p>
{/snippet}

{#snippet restartControl()}
	<button type="button" data-testid="library-restart" disabled={!$player.restartAvailable} aria-describedby={$player.restartAvailable ? undefined : `${id}-restart-hint`} onclick={() => player.restart()}>Restart current track</button>
	{#if !$player.restartAvailable}<p id={`${id}-restart-hint`}>Restart is unavailable until this track can seek.</p>{/if}
{/snippet}

{#snippet restartIconControl()}
	<button type="button" data-testid="library-restart" aria-label="Restart current track" title="Restart current track" disabled={!$player.restartAvailable} aria-describedby={$player.restartAvailable ? undefined : `${id}-restart-hint`} onclick={() => player.restart()}>
		<IconSvg path={TRANSPORT_ICON_PATHS.restart} />
	</button>
{/snippet}

<section use:mountFullscreen class:expanded aria-label="Player" data-testid="player-bar" data-player-transport tabindex="-1">
	{#if expanded}
		<div class="cinema-artwork"><Artwork src={selected?.artworkUrl} /></div>
	{/if}
	{#if !expanded}
		<div data-testid="mini-player" class="mini-wrap">
			<span class="mini-progress" aria-hidden="true"><span data-testid="mini-player-progress" style:width={`${progress}%`}></span></span>
			<div class="mini-surface">
				{#if selected}
					<a class="identity-link" href="/now-playing" data-testid="player-now-playing-toggle" aria-label={`Now playing: ${displayTitle(selected)}${displayArtist(selected) ? ` — ${displayArtist(selected)}` : ''}`} onclick={noteOpen}>
						<span class="compact-art" aria-hidden="true"><Artwork src={selected.artworkUrl} /></span>
						<span class="identity" data-testid="library-now-playing">{displayTitle(selected)}{#if displayArtist(selected)}{' — '}{displayArtist(selected)}{/if}</span>
					</a>
					<p class="identity compact-hide" data-testid="now-playing" role={$player.queueLiveRegions ? 'status' : undefined}>Now playing: {displayTitle(selected)}{#if selected.artist}{' — '}{selected.artist}{/if}</p>
					{@render favoriteControl()}
					{#if compact}{@render restartIconControl()}{/if}
					{@render playControl()}
				{/if}
				{@render queueToggle?.(() => fullscreenController?.exit() ?? Promise.resolve(false))}
			</div>
		</div>
	{/if}
	<div class="controls full-controls">
	<h2 class="compact-hide">Now playing</h2>
	{#if selected}
		{#if expanded}
			<p class="identity" data-testid="library-now-playing">
				{displayTitle(selected)}{#if displayArtist(selected)}{' — '}{displayArtist(selected)}{/if}
			</p>
			<p class="identity" data-testid="now-playing" role={$player.queueLiveRegions ? 'status' : undefined}>Now playing: {displayTitle(selected)}{#if selected.artist}{' — '}{selected.artist}{/if}</p>
		{/if}
	{/if}
	{#if expanded}{@render seekControl()}{/if}
	<div class="transport-buttons">
		<button type="button" aria-label="Shuffle upcoming tracks" title="Shuffle upcoming tracks" onclick={() => player.shuffle()} disabled={$player.queue.tracks.length - $player.queue.currentIndex < 3}>
			<IconSvg path={TRANSPORT_ICON_PATHS.shuffle} />
		</button>
		<button type="button" aria-label="Previous track" title="Previous track" onclick={() => void player.previous()}><IconSvg path={TRANSPORT_ICON_PATHS['skip-back']} /></button>
		{#if expanded}{@render restartIconControl()}{/if}
		{#if expanded}{@render playControl()}{/if}
		<button type="button" data-testid="player-next" aria-label="Next track" title="Next track" onclick={() => void player.next()}><IconSvg path={TRANSPORT_ICON_PATHS['skip-forward']} /></button>
		<button type="button" aria-label={`Repeat: ${$player.repeat === 'off' ? 'Off' : $player.repeat === 'queue' ? 'Queue' : 'One'}`} title="Cycle repeat" aria-pressed={$player.repeat !== 'off'} onclick={() => player.cycleRepeat()}>
			<IconSvg path={TRANSPORT_ICON_PATHS[$player.repeat === 'one' ? 'repeat-1' : 'repeat']} />
		</button>
	</div>
	{#if expanded && !$player.restartAvailable}<p id={`${id}-restart-hint`}>Restart is unavailable until this track can seek.</p>{/if}
	<div class="compact-hide">{#if !expanded && !compact}{@render restartControl()}{/if}</div>
	{#if !expanded}{@render seekControl()}{/if}
	<div class="sound">
		{#if expanded && selected}{@render favoriteControl()}{/if}
		<button class="compact-hide" type="button" aria-label={transport?.sound.muted ? 'Unmute' : 'Mute'} aria-pressed={transport?.sound.muted ?? false} onclick={() => player.toggleMute()}>
			<IconSvg path={TRANSPORT_ICON_PATHS[transport?.sound.muted ? 'volume-x' : 'volume-2']} />
		</button>
		<label class="compact-hide" for={`${id}-volume`}>Volume</label>
		<input class="compact-hide" id={`${id}-volume`} data-testid="player-volume" type="range" min="0" max="100" step="5" value={volume} aria-valuetext={`${volume}%`} oninput={(event) => player.setVolume(Number(event.currentTarget.value) / 100)} />
		{#if expanded}{@render queueToggle?.(() => fullscreenController?.exit() ?? Promise.resolve(false))}{/if}
		{#if expanded}<button type="button" data-testid="player-fullscreen" aria-label={fullscreen.active ? 'Exit full screen' : 'Enter full screen'} aria-pressed={fullscreen.active} aria-describedby={!fullscreen.supported ? `${id}-fullscreen-hint` : undefined} disabled={!fullscreen.supported || fullscreen.pending} onclick={() => void fullscreenController?.toggle()}>{fullscreen.active ? 'Exit full screen' : 'Enter full screen'}</button>{/if}
		<button class="compact-hide" type="button" aria-haspopup="dialog" onclick={() => void openHelp()}>Keyboard shortcuts</button>
	</div>
	<details class="compact-hide">
		<summary>Playback</summary>
		<div class="playback-settings">
			<label for={`${id}-speed`}>Playback speed</label>
			<select id={`${id}-speed`} value={transport?.rate ?? 1} onchange={(event) => player.setSpeed(Number(event.currentTarget.value))}>
				{#each [0.5, 0.75, 1, 1.25, 1.5, 1.75, 2] as rate}<option value={rate}>{rate}×</option>{/each}
			</select>
			<label for={`${id}-sleep`}>Sleep timer</label>
			<select id={`${id}-sleep`} value={!sleep ? 'off' : sleep.kind === 'end-of-track' ? 'end' : sleep.kind === 'duration' ? String(Math.ceil(sleep.remainingMs / 60000)) : 'off'} onchange={onSleepChange}>
				<option value="off">Off</option>
				{#each [15, 30, 60] as minutes}<option value={String(minutes)}>{minutes} minutes</option>{/each}
				<option value="end">End of track</option>
				{#if sleep?.kind === 'duration' && ![15, 30, 60].includes(Math.ceil(sleep.remainingMs / 60000))}
					<option value={String(Math.ceil(sleep.remainingMs / 60000))}>{sleepText}</option>
				{/if}
			</select>
			<p>{sleepText}</p>
		</div>
	</details>
	{#if transport?.sound.restricted}<p class="compact-hide">Volume control is limited by this browser. Use your device volume controls.</p>{/if}
	{#if transport?.status === 'buffering' || transport?.status === 'loading'}<p class="compact-hide" role="status">{transport.status === 'buffering' ? 'Buffering…' : 'Loading track…'}</p>{/if}
	{#if transport?.status === 'error'}
		<p class="compact-hide" role="alert">Could not play this track. Check your connection and try again.</p>
		<button class="compact-hide" type="button" onclick={() => void player.retry()}>Retry playback</button>
	{/if}
	<p class="compact-hide" aria-live="polite">{feedback}</p>
	{#if expanded && !fullscreen.supported}<p id={`${id}-fullscreen-hint`}>Full screen is unavailable in this browser.</p>{/if}
	{#if fullscreen.failed}<p class="compact-hide" role="status">Could not change full screen. Try again or use your browser controls.</p>{/if}

{#if helpOpen}
	<dialog bind:this={dialog} data-testid="shortcuts-dialog" aria-labelledby={`${id}-help`} oncancel={(event) => { event.preventDefault(); closeHelp(); }} onkeydown={onDialogKey}>
		<h2 id={`${id}-help`}>Keyboard shortcuts</h2>
		<p>Shortcuts are inactive while typing, adjusting a slider, or using another dialog.</p>
		<dl>{#each PLAYER_SHORTCUT_ROWS as row}<dt><kbd>{row.key}</kbd></dt><dd>{row.key === 'r' ? 'Cycle repeat: Off / Queue / One' : row.label}</dd>{/each}</dl>
		<p>Space on the player or main view also plays or pauses. Shift+J/L seeks 30 seconds. Alt+↑/↓ changes volume by 5%.</p>
		<button type="button" onclick={closeHelp}>Close keyboard shortcuts</button>
	</dialog>
{/if}
	</div>
	<p class:sr-only={!expanded && compact} data-testid="library-restart-status" aria-live="polite">{#key $player.restartVersion}{$player.restartStatus}{/key}</p>
	{#if !expanded && compact && !$player.restartAvailable}<p id={`${id}-restart-hint`} class="sr-only">Restart is unavailable until this track can seek.</p>{/if}
</section>

<style>
	.sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0 0 0 0); clip-path: inset(50%); white-space: nowrap; border: 0; }
	.mini-wrap { display: contents; }
	.mini-progress { display: none; }
	.mini-surface { display: contents; }
	.expanded:not(:fullscreen) { display: flex; flex-direction: column; align-items: center; gap: var(--nt-space-3); }
	.expanded:not(:fullscreen) .controls { width: 100%; min-width: 0; }
	.expanded:not(:fullscreen) :where(button, input, summary, select) { scroll-margin-block: calc(var(--nt-control-lg) + var(--nt-space-2)); }
	section:fullscreen { display: flex; flex-direction: column; align-items: center; justify-content: safe center; width: 100%; height: 100%; overflow: auto; box-sizing: border-box; gap: var(--nt-space-3); }
	.cinema-artwork { flex: 0 1 auto; min-height: 0; width: min(58dvh, 100%); aspect-ratio: 1; }
	.cinema-artwork :global(img), .cinema-artwork :global([role='img']) { display: block; width: 100%; height: 100%; object-fit: contain; border-radius: var(--nt-radius-pane); }
	section:fullscreen .controls { flex: 0 0 auto; width: min(100%, 64rem); background: var(--nt-bg-raised); }
	@media (min-width: 768px) and (hover: hover) and (pointer: fine) and (prefers-reduced-motion: no-preference) {
		section:fullscreen .cinema-artwork { position: absolute; top: 50%; left: 50%; transform: translate(-50%, -50%); height: 58dvh; width: min(58dvh, 100%); }
		section:fullscreen .controls { position: relative; margin-top: auto; z-index: 1; }
	}
	.identity-link { display: flex; align-items: center; gap: var(--nt-space-2); min-width: 0; color: var(--nt-fg); text-decoration: none; }
	.compact-art { flex: 0 0 var(--nt-space-px-40); width: var(--nt-space-px-40); height: var(--nt-space-px-40); }
	.compact-art :global(img), .compact-art :global([role='img']) { display: block; width: 100%; height: 100%; object-fit: cover; border-radius: var(--nt-radius-sm); }
	.identity { overflow-wrap: anywhere; }
	section { padding: var(--nt-space-2) var(--nt-space-3); background: var(--nt-bg-raised); border-top: 1px solid var(--nt-border); color: var(--nt-fg); }
	.transport-buttons, .sound, .seek { display: flex; align-items: center; justify-content: center; gap: var(--nt-space-2); }
	.transport-buttons { flex-wrap: wrap; }
	button { display: inline-flex; align-items: center; justify-content: center; min-height: var(--nt-control); min-width: var(--nt-control); padding: var(--nt-space-2); border: 0; border-radius: var(--nt-radius-pill); background: var(--nt-surface-tint); color: var(--nt-fg); font: inherit; cursor: pointer; }
	button:disabled { opacity: 0.5; cursor: default; }
	button[aria-pressed='true'] { color: var(--nt-accent); }
	.play { width: var(--nt-control-lg); height: var(--nt-control-lg); background: var(--nt-accent); color: var(--nt-accent-fg); }
	.seek { margin-block: var(--nt-space-2); font-size: var(--nt-text-xs); font-variant-numeric: tabular-nums; }
	.seek input { flex: 1; min-width: 0; max-width: 40rem; }
	input[type='range'] { min-height: var(--nt-space-px-24); accent-color: var(--nt-accent); }
	.sound { flex-wrap: wrap; }
	.sound input { width: min(8rem, 35vw); }
	details { margin-top: var(--nt-space-2); }
	summary { cursor: pointer; min-height: var(--nt-control); align-content: center; }
	.playback-settings { display: flex; flex-wrap: wrap; align-items: center; gap: var(--nt-space-2); }
	select { min-height: var(--nt-control); background: var(--nt-bg); color: var(--nt-fg); border: 1px solid var(--nt-border); border-radius: var(--nt-radius-sm); padding: var(--nt-space-2); font: inherit; }
	:where(button, input, summary, select):focus-visible { outline: var(--nt-focus-ring-width) solid var(--nt-focus-ring); outline-offset: var(--nt-focus-ring-offset); }
	@media (max-width: 799px) {
		/* The section is the sticky element: a sticky child inside a
		 * 56px-tall section cannot stick beyond it. */
		section:not(.expanded) { position: sticky; bottom: var(--nt-control); z-index: var(--nt-z-player); display: block; height: var(--nt-control-lg); padding: 0; border-top: 0; }
		section:not(.expanded) .full-controls { display: none; }
		.mini-wrap {
			display: block;
			height: var(--nt-control-lg);
			background: var(--nt-bg-raised);
			border-top: 1px solid var(--nt-border);
		}
		.mini-progress { position: absolute; inset: 0 0 auto; display: block; height: var(--nt-space-px-2); background: var(--nt-border); pointer-events: none; }
		.mini-progress span { display: block; height: 100%; background: var(--nt-accent); }
		.mini-surface { display: flex; align-items: center; gap: var(--nt-space-1); height: 100%; padding: var(--nt-space-1) var(--nt-space-2); }
		.mini-surface .compact-hide { display: none !important; }
		.mini-surface .identity-link { flex: 1; min-height: 2.75rem; }
		.mini-surface .identity-link .identity { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; font-size: var(--nt-text-sm); }
	}
	p:empty { display: none; }
	dialog { max-width: min(38rem, calc(100vw - var(--nt-space-4))); max-height: 80dvh; padding: var(--nt-space-4); border: 1px solid var(--nt-border); border-radius: var(--nt-radius-dialog); background: var(--nt-bg-raised); color: var(--nt-fg); }
	dialog::backdrop { background: var(--nt-scrim); }
	dl { display: grid; grid-template-columns: auto 1fr; gap: var(--nt-space-2) var(--nt-space-3); }
	dd { margin: 0; }
</style>
