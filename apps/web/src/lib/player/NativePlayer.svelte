<script lang="ts">
	import type { ComponentProps } from 'svelte';
	import { displayArtist, displayTitle } from '../library/order.js';
	import NativeAudio from './NativeAudio.svelte';
	import PlayerSeek from './PlayerSeek.svelte';
	import type { PlaybackStatus } from './mediaController.js';
	import type { Track } from './queue.js';
	import type { JumpDirection } from './relativeJump.js';

	let {
		currentTrack, mediaRevision, streamUrl, attachNativeAudio,
		playbackStatus, playCurrent, pauseCurrent,
		seekState, seek,
		restartCanSeek, restartStatus, restartStatusVersion, restartCurrentTrack,
		jumpStatus, jumpStatusVersion, jumpRelative
	}: ComponentProps<typeof NativeAudio> & {
		currentTrack: Pick<Track, 'id' | 'title' | 'artist'> | null;
		seekState: ComponentProps<typeof PlayerSeek>['state'];
		seek: ComponentProps<typeof PlayerSeek>['seek'];
		playbackStatus: PlaybackStatus;
		playCurrent: () => Promise<void>;
		pauseCurrent: () => void;
		restartCanSeek: boolean;
		restartStatus: string;
		restartStatusVersion: number;
		restartCurrentTrack: () => void;
		jumpStatus: string;
		jumpStatusVersion: number;
		jumpRelative: (direction: JumpDirection) => void;
	} = $props();
	const canPause = $derived(playbackStatus === 'playing' || playbackStatus === 'starting');
	const statusText: Record<PlaybackStatus, string> = {
		unavailable: 'Playback is unavailable.',
		paused: 'Paused',
		starting: 'Starting playback…',
		playing: 'Playing',
		blocked: 'Playback was blocked. Press Play to try again.',
		error: 'This audio could not be loaded or played. Choose another track.'
	};
</script>

<section aria-label="Player">
	<h2>Now playing</h2>
	{#if currentTrack}
		<p class="nt-player-identity" data-testid="library-now-playing">
			{displayTitle(currentTrack)}{#if displayArtist(currentTrack)}{' — '}{displayArtist(currentTrack)}{/if}
		</p>
	{/if}
	<div class="nt-library-player-controls">
		<button class="nt-button nt-library-transport" type="button"
			disabled={playbackStatus === 'error' || playbackStatus === 'unavailable'}
			onclick={() => canPause ? pauseCurrent() : void playCurrent()}>
			{canPause ? 'Pause' : 'Play'}
		</button>
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
	{#key seekState.attachment}
		<PlayerSeek state={seekState} {seek} />
	{/key}
	<p role="status" aria-atomic="true">{statusText[playbackStatus]}</p>
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
	.nt-library-transport,
	.nt-library-restart {
		flex: 0 0 auto;
		min-height: 2.75rem;
		border: 0;
		cursor: pointer;
		font: inherit;
	}
	.nt-library-transport:disabled,
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
