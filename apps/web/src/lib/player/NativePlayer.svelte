<script lang="ts">
	import type { ComponentProps } from 'svelte';
	import { displayArtist, displayTitle } from '../library/order.js';
	import NativeAudio from './NativeAudio.svelte';
	import type { Track } from './queue.js';
	import type { JumpDirection } from './relativeJump.js';

	let {
		currentTrack, mediaRevision, streamUrl, attachNativeAudio,
		restartCanSeek, restartStatus, restartStatusVersion, restartCurrentTrack,
		jumpStatus, jumpStatusVersion, jumpRelative
	}: ComponentProps<typeof NativeAudio> & {
		currentTrack: Pick<Track, 'id' | 'title' | 'artist'> | null;
		restartCanSeek: boolean;
		restartStatus: string;
		restartStatusVersion: number;
		restartCurrentTrack: () => void;
		jumpStatus: string;
		jumpStatusVersion: number;
		jumpRelative: (direction: JumpDirection) => void;
	} = $props();
</script>

<section aria-label="Player">
	<h2>Now playing</h2>
	{#if currentTrack}
		<p class="nt-player-identity" data-testid="library-now-playing">
			{displayTitle(currentTrack)}{#if displayArtist(currentTrack)}{' — '}{displayArtist(currentTrack)}{/if}
		</p>
	{/if}
	<div class="nt-library-player-controls">
		<NativeAudio {mediaRevision} {streamUrl} {attachNativeAudio} />
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
	</div>
	{#if !restartCanSeek}
		<p id="library-restart-hint">Restart is unavailable until this track can seek.</p>
	{/if}
	<p data-testid="library-restart-status" aria-live="polite">{#key restartStatusVersion}{restartStatus}{/key}</p>
	<div class="nt-player-jump">
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
</section>

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
	.nt-library-restart {
		flex: 0 0 auto;
		min-height: 2.75rem;
		border: 0;
		cursor: pointer;
		font: inherit;
	}
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
</style>
