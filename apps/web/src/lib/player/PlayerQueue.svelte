<script lang="ts">
	import { tick } from 'svelte';
	import { currentTrack, type QueueState, type Track } from './queue.js';
	import ShuffleUpcoming from './ShuffleUpcoming.svelte';

	interface Props {
		state: QueueState;
		/** Transport stays with the host page: called when Next is activated. */
		onnext?: () => void;
		/** Queue-only edit: the host must not restart playback. */
		onshuffle?: () => void;
		radioActive?: boolean;
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
		/**
		 * Transient queue-only filter text. Bindable so a host that
		 * unmounts/remounts the panel (library refresh swaps branches)
		 * can keep the query across mounts; unbound parents get the same
		 * component-local default. Never persisted or fetched.
		 */
		filterQuery?: string;
	}

	// Renamed locally: a `state` binding would shadow the `$state` rune.
	let {
		state: queue,
		onnext,
		onshuffle,
		radioActive = false,
		onmove,
		onplaynext,
		onremove,
		onclear,
		announcement = '',
		filterQuery = $bindable('')
	}: Props = $props();

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

	let normalizedQuery = $derived(filterQuery.trim().toLowerCase());

	/** Blank titles render (and match) as their stable track id, never empty. */
	function displayTitle(track: Track): string {
		return track.title?.trim() ? track.title : track.id;
	}

	function matchesQuery(track: Track, normalized: string): boolean {
		if (!normalized) return true;
		const haystack = `${displayTitle(track)} ${track.artist ?? ''}`.toLowerCase();
		return haystack.includes(normalized);
	}

	/**
	 * Visible rows with their ORIGINAL queue indices: every row action,
	 * disabled boundary and the current marker use the full-queue slot, never
	 * the filtered position. Pure reads only — frozen props stay untouched.
	 */
	let visibleEntries = $derived(
		queue.tracks
			.map((track, index) => ({ track, index }))
			.filter(({ track }) => matchesQuery(track, normalizedQuery))
	);

	/**
	 * Explicit reveal only: bound to the Show-current-track button's click.
	 * Clears a hiding filter first, then focuses/reveals the real row.
	 * Never runs from an effect, never mutates state, never calls transport.
	 * Focus uses preventScroll and the scroll is instant (`block: nearest`,
	 * never smooth), so no reduced-motion special case is needed.
	 */
	async function revealCurrent(): Promise<void> {
		if (selectedIndex < 0) return;
		filterQuery = '';
		await tick();
		const row = list?.querySelector<HTMLElement>(
			'[data-testid="queue-item"][aria-current="true"]'
		);
		if (!row) return;
		row.focus({ preventScroll: true });
		row.scrollIntoView({ block: 'nearest', behavior: 'instant' });
	}
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

<p data-testid="queue-announcement" role="status">{announcement}</p>

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

<ShuffleUpcoming state={queue} {onshuffle} {radioActive} />

<!--
	Queue-only filter: transient presentation state. It hides rows but
	never reorders, reselects, persists or fetches anything; the
	orientation summary above always describes the full queue.
-->
<div class="queue-filter">
	<label for="queue-filter-input">Filter queue</label>
	<input id="queue-filter-input" type="search" bind:value={filterQuery} />
	<button type="button" onclick={() => (filterQuery = '')}>Clear queue filter</button>
</div>

<p data-testid="queue-filter-count" role="status">
	{visibleEntries.length} of {trackCount} tracks
</p>

{#if trackCount > 0 && visibleEntries.length === 0}
	<p data-testid="queue-filter-empty">No matches in this queue</p>
{/if}

<ol aria-label="Playback queue" bind:this={list}>
	<!-- Stable track identity preserves keyboard focus through reorders. The
		store guarantees uniqueness: addTrack dedupes and restoreQueue rejects duplicates. -->
	{#each visibleEntries as { track, index } (track.id)}
		<!-- svelte-ignore a11y_no_noninteractive_tabindex: tabindex is provably -1-or-absent (QO-U14); the current row is a programmatic focus target, never a tab stop. -->
		<li
			class="queue-row"
			data-testid="queue-item"
			data-track-id={track.id}
			aria-current={index === selectedIndex ? 'true' : undefined}
			tabindex={index === selectedIndex ? -1 : undefined}
		>
			{displayTitle(track)}{#if track.artist} — {track.artist}{/if}
			<button
				type="button"
				aria-label="Move {track.title} up"
				disabled={index === 0}
				onclick={() => onmove?.(index, index - 1)}
			>
				Up
			</button>
			<button
				type="button"
				aria-label="Move {track.title} down"
				disabled={index === trackCount - 1}
				onclick={() => onmove?.(index, index + 1)}
			>
				Down
			</button>
			<button
				type="button"
				aria-label="Play {track.title} next"
				onclick={() => onplaynext?.(index)}
			>
				Next
			</button>
			<button
				type="button"
				aria-label="Remove {track.title} from queue"
				onclick={() => onremove?.(index)}
			>
				Remove
			</button>
		</li>
	{/each}
</ol>

<button
	type="button"
	disabled={queue.tracks.length === 0}
	onclick={() => onclear?.()}
>
	Clear queue
</button>

<style>
	.queue-filter {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--nt-space-2);
	}

	.queue-filter input {
		flex: 1 1 12rem;
		min-width: 0;
		max-width: 100%;
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}

	/* Visible keyboard focus ring even where the app stylesheet is absent
	 * (mounted test parent); the app's own :focus-visible agrees with this. */
	.queue-filter input:focus-visible {
		outline: 2px solid var(--nt-accent);
		outline-offset: 2px;
	}

	/* Touch-sized clear action matching the input row. */
	.queue-filter button {
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
	}

	.queue-row {
		/* Clear the 2px outline + 2px offset when nearest-scroll rounds
		 * fractional line heights at the viewport edge. */
		scroll-margin-block: 4px;
	}

	/* Programmatic focus target for explicit reveal: the ring must hold
	 * even where the app stylesheet is absent (mounted test parent). */
	.queue-row:focus {
		outline: 2px solid var(--nt-accent, currentColor);
		outline-offset: 2px;
	}
</style>
