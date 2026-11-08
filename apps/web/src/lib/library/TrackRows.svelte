<script lang="ts">
	import { libraryItemKey, type LibraryItem } from './cache.js';
	import { displayArtist, displayTitle } from './order.js';
	import { contextMenu } from '#lib/interactions/context-menu.js';
	import { startTrackDrag } from '#lib/interactions/track-drag.js';

	interface Props {
		tracks: LibraryItem[];
		queuedIds: ReadonlySet<string>;
		favoriteIds: ReadonlySet<string>;
		selectedIds: string[];
		openTrackMenu: (track: LibraryItem, invoker: HTMLElement, point?: { x: number; y: number }) => void;
		libraryRowClick: (event: MouseEvent, id: string) => void;
		interactionIds: (id: string) => string[];
		ondragitem: (track: LibraryItem | null) => void;
		activateSelectionRow: (id: string, extend: boolean, box: HTMLInputElement) => void;
		selectTrack: (id: string, track: LibraryItem) => void;
		copyTrackLink: (id: string, track: LibraryItem) => Promise<void>;
		starLabel: (track: LibraryItem) => string;
		toggleFavorite: (id: string) => void;
	}

	let { tracks, queuedIds, favoriteIds, selectedIds, openTrackMenu, libraryRowClick,
		interactionIds, ondragitem, activateSelectionRow, selectTrack, copyTrackLink,
		starLabel, toggleFavorite }: Props = $props();
	let extendSelection = false;
</script>

	<ul class="nt-track-list" role="list">
		{#each tracks as track (libraryItemKey(track))}
			{@const queued = queuedIds.has(track.id)}
			<!-- svelte-ignore a11y_no_noninteractive_tabindex: programmatic menu-return target only. -->
			<!-- svelte-ignore a11y_no_noninteractive_element_interactions: modifier clicks extend the checkbox selection; keyboard equivalents are list-scoped above. -->
			<li class="nt-library-item" data-testid="library-item" data-id={track.id}
				tabindex="-1"
				data-selected={selectedIds.includes(track.id) ? '' : undefined}
				use:contextMenu={(invoker, point) => openTrackMenu(track, invoker, point)}
				onclickcapture={(event) => libraryRowClick(event, track.id)}
				draggable="true"
				ondragstart={(event) => {
					ondragitem(track);
					startTrackDrag(event, interactionIds(track.id));
				}}
				ondragend={() => { ondragitem(null); }}
			>
				<input
					class="nt-library-select"
					type="checkbox"
					aria-label={`Select ${displayTitle(track)}`}
					checked={selectedIds.includes(track.id)}
					onclick={(event) => {
						event.stopPropagation();
						extendSelection = event.shiftKey;
					}}
					onchange={(event) => {
						const extend = extendSelection;
						extendSelection = false;
						activateSelectionRow(track.id, extend, event.currentTarget);
					}}
				/>
				<button
					type="button"
					class="nt-library-row"
					data-context-menu-longpress
					aria-label={queued
						? `${displayTitle(track)}${displayArtist(track) ? ` — ${displayArtist(track)}` : ''} — Queued`
						: undefined}
					aria-pressed={queued ? true : undefined}
					onclick={() => selectTrack(track.id, track)}
				>
					{displayTitle(track)}
					{#if displayArtist(track)}
						<span> — {displayArtist(track)}</span>
					{/if}
					{#if queued}
						<span class="nt-queued-badge" data-testid="library-queued-badge">Queued</span>
					{/if}
				</button>
				<button type="button" class="nt-library-more"
					data-testid="track-more" aria-label={`More actions for ${displayTitle(track)}`}
					aria-haspopup="menu"
					onclick={(event) => openTrackMenu(track, event.currentTarget)}
				><svg aria-hidden="true" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="1" /><circle cx="19" cy="12" r="1" /><circle cx="5" cy="12" r="1" /></svg></button>
				<button
					type="button"
					class="nt-library-copy"
					data-testid="library-copy-link"
					aria-label={`Copy link ${displayTitle(track)}`}
					onclick={() => void copyTrackLink(track.id, track)}
				>
					Copy link
				</button>
				<button
					type="button"
					class="nt-library-star"
					aria-pressed={favoriteIds.has(track.id)}
					aria-label={starLabel(track)}
					onclick={() => toggleFavorite(track.id)}
				>
					<span aria-hidden="true">{favoriteIds.has(track.id) ? '★' : '☆'}</span>
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
	@media (pointer: coarse) { .nt-library-more { min-width: 48px; min-height: 48px; } }
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
