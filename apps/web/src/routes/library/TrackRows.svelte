<script lang="ts">
	import { libraryItemKey, type LibraryItem } from '#lib/library/cache.js';
	import { displayArtist, displayTitle } from '#lib/library/order.js';
	import { contextMenu } from '#lib/interactions/context-menu.js';
	import TrackLikeButton from '#lib/library/TrackLikeButton.svelte';
	import type { TrackLikeControls } from '#lib/library/track-like-controls.js';

	let {
		tracks,
		queuedIds,
		selectedIds,
		favoriteIds,
		onrowclick,
		onselectionclick,
		onselectionchange,
		ontrackdragstart,
		ontrackdragend,
		ontrackplay,
		ontrackmenu,
		oncopylink,
		ontogglefavorite,
		likeControls
	}: {
		tracks: readonly LibraryItem[];
		queuedIds: ReadonlySet<string>;
		selectedIds: readonly string[];
		favoriteIds: ReadonlySet<string>;
		onrowclick: (event: MouseEvent, id: string) => void;
		onselectionclick: (event: MouseEvent) => void;
		onselectionchange: (id: string, box: HTMLInputElement) => void;
		ontrackdragstart: (event: DragEvent, track: LibraryItem) => void;
		ontrackdragend: () => void;
		ontrackplay: (track: LibraryItem) => void;
		ontrackmenu: (track: LibraryItem, invoker: HTMLElement, point?: { x: number; y: number }) => void;
		oncopylink: (track: LibraryItem) => void;
		ontogglefavorite: (id: string) => void;
		likeControls?: TrackLikeControls;
	} = $props();
</script>

<ul class="nt-track-list" role="list">
	{#each tracks as track (libraryItemKey(track))}
		{@const queued = queuedIds.has(track.id)}
		{@const selected = selectedIds.includes(track.id)}
		{@const favorite = favoriteIds.has(track.id)}
		{@const title = displayTitle(track)}
		{@const artist = displayArtist(track)}
		<!-- svelte-ignore a11y_no_noninteractive_tabindex: programmatic menu-return target only. -->
		<!-- svelte-ignore a11y_no_noninteractive_element_interactions: modifier clicks extend the checkbox selection; keyboard equivalents are list-scoped on the Library page. -->
		<li class="nt-library-item" data-testid="library-item" data-id={track.id}
			data-library-key={libraryItemKey(track)}
			tabindex="-1"
			data-selected={selected ? '' : undefined}
			use:contextMenu={(invoker, point) => ontrackmenu(track, invoker, point)}
			onclickcapture={(event) => {
				if (!(event.target instanceof Element && event.target.closest('.nt-track-like'))) {
					onrowclick(event, track.id);
				}
			}}
			draggable="true"
			ondragstart={(event) => ontrackdragstart(event, track)}
			ondragend={ontrackdragend}
		>
			<input
				class="nt-library-select"
				type="checkbox"
				aria-label={`Select ${title}`}
				checked={selected}
				onclick={onselectionclick}
				onchange={(event) => onselectionchange(track.id, event.currentTarget)}
			/>
			<button
				type="button"
				class="nt-library-row"
				data-context-menu-longpress
				aria-label={queued
					? `${title}${artist ? ` — ${artist}` : ''} — Queued`
					: undefined}
				aria-pressed={queued ? true : undefined}
				onclick={() => ontrackplay(track)}
			>
				{title}
				{#if artist}
					<span> — {artist}</span>
				{/if}
				{#if queued}
					<span class="nt-queued-badge" data-testid="library-queued-badge">Queued</span>
				{/if}
			</button>
			<button type="button" class="nt-library-more"
				data-testid="track-more" aria-label={`More actions for ${title}`}
				aria-haspopup="menu"
				onclick={(event) => ontrackmenu(track, event.currentTarget)}
			><svg aria-hidden="true" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="1" /><circle cx="19" cy="12" r="1" /><circle cx="5" cy="12" r="1" /></svg></button>
			<button
				type="button"
				class="nt-library-copy"
				data-testid="library-copy-link"
				aria-label={`Copy link ${title}`}
				onclick={() => oncopylink(track)}
			>
				Copy link
			</button>
			{#if likeControls}
				{@const control = likeControls(track.id)}
				{#if control}<TrackLikeButton {title} {control} />{/if}
			{/if}
			<button
				type="button"
				class="nt-library-star"
				aria-pressed={favorite}
				aria-label={`${favorite ? 'Unstar' : 'Star'} ${title}`}
				onclick={() => ontogglefavorite(track.id)}
			>
				<span aria-hidden="true">{favorite ? '★' : '☆'}</span>
			</button>
		</li>
	{/each}
</ul>

<style>
	.nt-library-item[data-selected] { background: var(--nt-bg-raised); outline: 1px solid var(--nt-accent); }
	.nt-track-list { padding: 0; list-style: none; }
	.nt-library-more { flex: 0 0 2.75rem; min-height: 40px; padding: 0.25rem; font: inherit; }
	@media (max-width: 480px) {
		.nt-track-list .nt-library-copy { width: 2.75rem; padding: 0.25rem; white-space: normal; }
	}
	[data-context-menu-longpress] { -webkit-touch-callout: none; user-select: none; }
	@media (pointer: coarse) {
		.nt-library-more { min-width: 48px; min-height: 48px; }
	}
	.nt-library-select {
		flex: 0 0 auto;
		width: 1.5rem;
		height: 1.5rem;
		margin: 0.5rem 0 0.5rem 0.75rem;
		accent-color: var(--nt-accent);
	}
	.nt-library-item {
		display: flex;
		align-items: center;
		padding: var(--nt-library-row-padding) 0;
	}
	.nt-library-row {
		display: block;
		flex: 1 1 auto;
		min-width: 0;
		min-height: 2.75rem;
		overflow-wrap: anywhere;
		width: 100%;
		text-align: left;
		background: none;
		border: 0;
		padding: 0.5rem 0.75rem;
		cursor: pointer;
		font: inherit;
		color: inherit;
	}
	.nt-library-copy,
	.nt-library-star {
		flex: 0 0 auto;
		min-width: 2.75rem;
		min-height: 2.75rem;
		background: none;
		border: 0;
		padding: 0.5rem 0.75rem;
		cursor: pointer;
		font: inherit;
		color: inherit;
	}
	.nt-library-copy {
		color: var(--nt-accent);
		white-space: nowrap;
	}
	.nt-queued-badge {
		display: inline-block;
		margin-left: var(--nt-space-2);
		padding: 0 var(--nt-space-2);
		border-radius: var(--nt-radius);
		background: var(--nt-accent);
		color: var(--nt-accent-fg);
		font-size: 0.75rem;
		line-height: 1.6;
		white-space: nowrap;
	}
</style>
