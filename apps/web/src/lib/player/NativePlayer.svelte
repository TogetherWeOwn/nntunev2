<script lang="ts">
	import { tick, type ComponentProps, type Snippet } from 'svelte';
	import { MediaQuery } from 'svelte/reactivity';
	import { displayArtist, displayTitle } from '../library/order.js';
	import NativeAudio from './NativeAudio.svelte';
	import PlayerSeek from './PlayerSeek.svelte';
	import type { PlaybackStatus } from './mediaController.js';
	import type { Track } from './queue.js';
	import type { JumpDirection } from './relativeJump.js';

	let {
		currentTrack, mediaRevision, streamUrl, attachNativeAudio,
		playbackStatus, playCurrent, pauseCurrent, retryCurrent,
		seekState, seek,
		restartCanSeek, restartStatus, restartStatusVersion, restartCurrentTrack,
		favoriteAction, allowDesktopRow = true,
		jumpStatus, jumpStatusVersion, jumpRelative, volumeControls
	}: ComponentProps<typeof NativeAudio> & {
		volumeControls: Snippet<['controls' | 'status']>;
		currentTrack: Pick<Track, 'id' | 'title' | 'artist'> | null;
		seekState: ComponentProps<typeof PlayerSeek>['state'];
		seek: ComponentProps<typeof PlayerSeek>['seek'];
		playbackStatus: PlaybackStatus;
		playCurrent: () => Promise<void>;
		pauseCurrent: () => void;
		retryCurrent: () => Promise<void>;
		restartCanSeek: boolean;
		restartStatus: string;
		restartStatusVersion: number;
		restartCurrentTrack: () => void;
		jumpStatus: string;
		jumpStatusVersion: number;
		jumpRelative: (direction: JumpDirection) => void;
		favoriteAction?: Snippet<[statusOnly: boolean]>;
		allowDesktopRow?: boolean;
	} = $props();
	// Match the shell's mobile cutoff. Only the accessory controls change hosts.
	const wideShell = new MediaQuery('(width > 799px)');
	const desktop = $derived(allowDesktopRow && wideShell.current);
	let row = $state<HTMLDivElement>(), passive = $state<HTMLDivElement>(), jumps = $state<HTMLDivElement>();
	$effect(() => {
		if (!row || !passive || !jumps) return;
		// Match reading order without remounting the live regions or moving Seek and the jumps.
		const before = desktop ? null : jumps;
		if (passive.nextElementSibling === before) return;
		if (row.moveBefore) row.moveBefore(passive, before);
		else row.insertBefore(passive, before);
	});
	let wasDesktop = false;
	$effect.pre(() => {
		if (desktop === wasDesktop) return;
		wasDesktop = desktop;
		const active = document.activeElement;
		const host = row;
		if (!active || !host?.contains(active)) return;
		const selector = ['[data-testid="player-favorite"]', '.nt-sound input', '.nt-sound button']
			.find((candidate) => active.matches(candidate));
		if (!selector) return;
		void tick().then(() => {
			// Don't steal focus if the user has already moved to a different control.
			if (!active.isConnected && document.activeElement === document.body) {
				host.querySelector<HTMLElement>(selector)?.focus();
			}
		});
	});
	const canPause = $derived(playbackStatus === 'playing' || playbackStatus === 'starting');
	const statusText: Record<PlaybackStatus, string> = {
		unavailable: 'Playback is unavailable.',
		paused: 'Paused',
		starting: 'Starting playback…',
		playing: 'Playing',
		blocked: 'Playback was blocked. Press Play to try again.',
		error: 'This audio could not be loaded or played. Retry playback or choose another track.',
		retrying: 'Retrying playback…'
	};
</script>

<section aria-label="Player">
	<h2>Now playing</h2>
	<div class="nt-player-row" class:desktop bind:this={row}>
		<div class="nt-player-left">
			{#if currentTrack}
				<p class="nt-player-identity" data-testid="library-now-playing">
					{displayTitle(currentTrack)}{#if displayArtist(currentTrack)}{' — '}{displayArtist(currentTrack)}{/if}
				</p>
			{/if}
			{#if desktop}{@render favoriteAction?.(false)}{/if}
		</div>
		<div class="nt-player-primary">
			{@render transport()}
			{#key seekState.attachment}<PlayerSeek state={seekState} {seek} />{/key}
		</div>
		<div class="nt-player-passive" bind:this={passive}>
			{#key seekState.attachment}
				<p class="nt-seek-feedback" aria-live="polite" aria-atomic="true">
					{#if seekState.seeking}<span aria-hidden="true">Seeking… </span>{/if}
					{#key seekState.feedbackVersion}{#if seekState.feedback}<span>{seekState.feedback}</span>{/if}{/key}
				</p>
			{/key}
			{#if !desktop}{@render volumeControls('controls')}{/if}
			{@render statusLines()}
		</div>
		{@render jumpExtension()}
		{#if desktop}
			<div class="nt-player-right">{@render volumeControls('controls')}</div>
		{/if}
	</div>
</section>

{#snippet transport()}
	<div class="nt-library-player-controls">
		<button class="nt-button nt-library-transport" type="button"
			disabled={playbackStatus === 'unavailable'}
			aria-disabled={playbackStatus === 'retrying' ? 'true' : undefined}
			onclick={() => {
				if (playbackStatus === 'retrying') return;
				if (playbackStatus === 'error') void retryCurrent();
				else if (canPause) pauseCurrent();
				else void playCurrent();
			}}>
			{playbackStatus === 'error' || playbackStatus === 'retrying' ? 'Retry playback' : canPause ? 'Pause' : 'Play'}
		</button>
		{#if streamUrl}<NativeAudio {mediaRevision} {streamUrl} {attachNativeAudio} />{/if}
		<button
			class="nt-button nt-library-restart"
			type="button"
			data-testid="library-restart"
			disabled={!restartCanSeek}
			aria-describedby={restartCanSeek ? undefined : 'library-restart-hint'}
			onclick={restartCurrentTrack}
		>
			Restart current track
		</button>
		{#if !desktop}{@render favoriteAction?.(false)}{/if}
		{@render favoriteAction?.(true)}
	</div>
{/snippet}

{#snippet statusLines()}
	<div class="nt-player-status">
		{@render volumeControls('status')}
		<p role="status" aria-atomic="true">{statusText[playbackStatus]}</p>
		{#if !restartCanSeek}
			<p id="library-restart-hint">Restart is unavailable until this track can seek.</p>
		{/if}
		<p data-testid="library-restart-status" aria-live="polite">{#key restartStatusVersion}{restartStatus}{/key}</p>
	</div>
{/snippet}

{#snippet jumpExtension()}
	<div class="nt-player-jump" bind:this={jumps}>
		<button
			class="nt-button nt-button--ghost"
			type="button"
			data-testid="player-seek-back"
			aria-label="Back 10 seconds"
			onclick={() => jumpRelative('back')}
		>
			Back 10 s
		</button>
		<button
			class="nt-button nt-button--ghost"
			type="button"
			data-testid="player-seek-forward"
			aria-label="Forward 10 seconds"
			onclick={() => jumpRelative('forward')}
		>
			Forward 10 s
		</button>
	</div>
	<p data-testid="player-seek-feedback" aria-live="polite">{#key jumpStatusVersion}{jumpStatus}{/key}</p>
{/snippet}

<style>
	.nt-player-identity {
		overflow-wrap: anywhere;
	}
	.nt-library-player-controls {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--nt-space-2) var(--nt-space-3);
	}
	.nt-library-transport,
	.nt-library-restart {
		flex: 0 0 auto;
		min-height: 2.75rem;
		border: 0;
		cursor: pointer;
		font: inherit;
	}
	.nt-library-transport:disabled,
	.nt-library-transport[aria-disabled='true'],
	.nt-library-restart:disabled {
		cursor: default;
		opacity: 0.5;
	}
	.nt-player-jump {
		display: flex;
		flex-wrap: wrap;
		gap: var(--nt-space-2) var(--nt-space-3);
		margin-block: var(--nt-space-2);
	}
	.nt-player-jump button {
		min-height: 2.75rem;
	}

	/* Ordinary mobile keeps its existing boxes and source order. */
	.nt-player-row, .nt-player-left, .nt-player-passive, .nt-player-primary, .nt-player-status {
		display: contents;
	}
	.nt-player-row.desktop {
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(0, 3fr) minmax(0, 1fr);
		column-gap: var(--nt-shell-gap);
	}
	.desktop :is(.nt-player-primary, .nt-player-jump, [data-testid='player-seek-feedback'], .nt-seek-feedback, .nt-player-status) {
		grid-column: 2;
	}
	.nt-seek-feedback:empty, .desktop .nt-seek-feedback { margin: 0; }
	.desktop .nt-player-status { display: block; }
	.desktop .nt-player-left, .desktop .nt-player-right {
		display: block;
		grid-row: 1;
		align-self: center;
	}
	.desktop .nt-player-left { grid-column: 1; }
	.desktop .nt-player-right { grid-column: 3; }
	.desktop .nt-player-primary {
		display: grid;
		/* Side metadata may grow the row; keep seek flush to the jump extension. */
		align-content: end;
		min-block-size: var(--nt-player-h);
	}
	.desktop .nt-player-identity, .desktop [data-testid='player-seek-feedback'], .desktop :global(.nt-sound) {
		margin: 0;
	}
	.desktop .nt-library-player-controls, .desktop .nt-player-jump { gap: var(--nt-space-2); }
	.desktop .nt-player-jump { margin-block-end: 0; }
	.desktop :is(.nt-library-player-controls, .nt-player-jump) button {
		min-block-size: var(--nt-control);
		min-inline-size: var(--nt-control);
		max-inline-size: 100%;
		padding-inline: var(--nt-space-2);
	}
	.desktop .nt-library-player-controls .nt-library-transport { min-block-size: var(--nt-control-lg); }
	/* Wrap at the browser's native control width, not the narrow flex basis. */
	.desktop .nt-library-player-controls :global(audio[controls]) { flex-basis: auto; }
	.desktop :global(.nt-sound input) { max-inline-size: 100%; }
	.desktop :global(.nt-sound label) { display: grid; }
	.desktop .nt-player-primary :global(.nt-player-seek) { line-height: 1; }
	.desktop .nt-player-primary :global(.nt-player-seek input) { min-height: var(--nt-space-px-12); }
</style>
