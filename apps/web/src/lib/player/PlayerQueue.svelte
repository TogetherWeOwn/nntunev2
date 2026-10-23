<script lang="ts">
	import { currentTrack, type QueueState } from './queue.js';

	interface Props {
		state: QueueState;
		/** Transport stays with the host page: called when Next is activated. */
		onnext?: () => void;
	}

	// Renamed locally: a `state` binding would shadow the `$state` rune.
	let { state: queue, onnext }: Props = $props();

	let current = $derived(currentTrack(queue));

	/**
	 * Defensive selected slot: only an in-range integer index counts as a
	 * selection. Anything else (empty queue, negative, past-end, fractional,
	 * NaN, infinite) renders the unselected summary and no current row —
	 * never a clamped or fabricated position.
	 */
	let selectedIndex = $derived(
		Number.isInteger(queue.currentIndex) &&
			queue.currentIndex >= 0 &&
			queue.currentIndex < queue.tracks.length
			? queue.currentIndex
			: -1
	);

	let trackCount = $derived(queue.tracks.length);
	let upcoming = $derived(selectedIndex < 0 ? 0 : trackCount - selectedIndex - 1);

	let list: HTMLOListElement | null = $state(null);

	/**
	 * Explicit reveal only: bound to the Show-current-track button's click.
	 * Never runs from an effect, never mutates state, never calls transport.
	 * Focus uses preventScroll and the scroll is instant (`block: nearest`,
	 * never smooth), so no reduced-motion special case is needed.
	 */
	function revealCurrent(): void {
		if (selectedIndex < 0) return;
		const row = list?.querySelector<HTMLElement>(
			'[data-testid="queue-item"][aria-current="true"]'
		);
		if (!row) return;
		row.focus({ preventScroll: true });
		row.scrollIntoView({ block: 'nearest' });
	}
</script>

<!--
	Reusable queue panel: renders a `QueueState` from the queue store with
	the same hooks the demo player and the queue-order specs rely on.
	The host page owns selection and transport (via `onnext`); the panel
	itself never mutates state.
-->
<p data-testid="now-playing" role="status">
	{#if current}
		Now playing: {current.title}{#if current.artist} — {current.artist}{/if}
	{:else}
		Queue is empty
	{/if}
</p>

<p data-testid="queue-position">
	{#if trackCount === 0}
		Queue selection: Queue is empty
	{:else if selectedIndex < 0}
		Queue selection: No track selected, {trackCount}
		{trackCount === 1 ? 'track' : 'tracks'}
	{:else}
		Queue selection: Track {selectedIndex + 1} of {trackCount}, {upcoming} upcoming
	{/if}
</p>

<button
	type="button"
	data-testid="player-next"
	disabled={!current}
	onclick={() => onnext?.()}
>
	Next
</button>

<button
	type="button"
	data-testid="queue-reveal-current"
	disabled={selectedIndex < 0}
	onclick={revealCurrent}
>
	Show current track
</button>

<ol aria-label="Playback queue" bind:this={list}>
	<!-- Keyed by queue position. Uniqueness comes from the store: addTrack
		dedupes and restoreQueue rejects duplicates. -->
	{#each queue.tracks as track, i (i)}
		<!-- svelte-ignore a11y_no_noninteractive_tabindex: tabindex is provably -1-or-absent (QO-U14); the current row is a programmatic focus target, never a tab stop. -->
		<li
			class="queue-row"
			data-testid="queue-item"
			data-track-id={track.id}
			aria-current={i === selectedIndex ? 'true' : undefined}
			tabindex={i === selectedIndex ? -1 : undefined}
		>
			{track.title}{#if track.artist} — {track.artist}{/if}
		</li>
	{/each}
</ol>

<style>
	/* Programmatic focus target for explicit reveal: the ring must hold
	 * even where the app stylesheet is absent (mounted test parent). */
	.queue-row:focus-visible {
		outline: 2px solid var(--nt-accent, currentColor);
		outline-offset: 2px;
	}
</style>
