<script lang="ts">
	import { currentTrack, type QueueState } from './queue.js';

	interface Props {
		state: QueueState;
		/** Transport stays with the host page: called when Next is activated. */
		onnext?: () => void;
		/** Queue management stays with the host page too: the panel reports intent, never mutates. */
		onmove?: (from: number, to: number) => void;
		onplaynext?: (index: number) => void;
		onremove?: (index: number) => void;
		onclear?: () => void;
		/**
		 * Polite move announcements, owned by the host: the host sets the
		 * moved track and its new position after each move; the panel only
		 * renders the region.
		 */
		announcement?: string;
	}

	let { state, onnext, onmove, onplaynext, onremove, onclear, announcement = '' }: Props =
		$props();

	let current = $derived(currentTrack(state));
</script>

<!--
	Reusable queue panel: renders a `QueueState` from the queue store with
	the same hooks the demo player and the queue-order specs rely on.
	The host page owns selection, transport (via `onnext`) and management
	(via `onmove`/`onplaynext`/`onremove`/`onclear`); the panel itself never
	mutates state.
-->
<p data-testid="now-playing" role="status">
	{#if current}
		Now playing: {current.title}{#if current.artist} — {current.artist}{/if}
	{:else}
		Queue is empty
	{/if}
</p>

<p data-testid="queue-announcement" role="status">{announcement}</p>

<button
	type="button"
	data-testid="player-next"
	disabled={!current}
	onclick={() => onnext?.()}
>
	Next
</button>

<ol aria-label="Playback queue">
	<!-- Stable track identity preserves keyboard focus through reorders. The
		store guarantees uniqueness: addTrack dedupes and restoreQueue rejects duplicates. -->
	{#each state.tracks as track, i (track.id)}
		<li
			data-testid="queue-item"
			data-track-id={track.id}
			aria-current={i === state.currentIndex ? 'true' : undefined}
		>
			{track.title}{#if track.artist} — {track.artist}{/if}
			<button
				type="button"
				aria-label="Move {track.title} up"
				disabled={i === 0}
				onclick={() => onmove?.(i, i - 1)}
			>
				Up
			</button>
			<button
				type="button"
				aria-label="Move {track.title} down"
				disabled={i === state.tracks.length - 1}
				onclick={() => onmove?.(i, i + 1)}
			>
				Down
			</button>
			<button
				type="button"
				aria-label="Play {track.title} next"
				onclick={() => onplaynext?.(i)}
			>
				Next
			</button>
			<button
				type="button"
				aria-label="Remove {track.title} from queue"
				onclick={() => onremove?.(i)}
			>
				Remove
			</button>
		</li>
	{/each}
</ol>

<button
	type="button"
	disabled={state.tracks.length === 0}
	onclick={() => onclear?.()}
>
	Clear queue
</button>
