<script lang="ts">
	import { libraryItemKey, type LibraryItem } from './cache.js';
	import { displayArtist, displayTitle } from './order.js';
	import type { FavoriteMoveDirection } from './favorites-order.js';
	import { contextMenu } from '#lib/interactions/context-menu.js';

	interface Props {
		favoriteTracks: LibraryItem[];
		favoritesAnnouncement: string;
		managementFeedback: string;
		favoritesExportStatusShown: string;
		favoritesExportFallbackShown: string | null;
		confirmClear: boolean;
		shelfSection?: HTMLElement | null;
		shelfHeading?: HTMLHeadingElement | null;
		clearButton?: HTMLButtonElement | null;
		openFavoriteMenu: (id: string, invoker: HTMLElement, point?: { x: number; y: number }) => void;
		selectTrack: (id: string, track: LibraryItem) => void;
		moveFavoriteLabel: (track: LibraryItem, direction: FavoriteMoveDirection) => string;
		moveFavorite: (id: string, direction: FavoriteMoveDirection, event: MouseEvent) => Promise<void>;
		unstarFromShelf: (id: string) => void;
		queueFavorites: () => void;
		copyFavoritesText: () => Promise<void>;
		downloadFavoritesM3U: () => void;
		requestClearFavorites: () => void;
		cancelClearFavorites: () => void;
		confirmClearFavorites: () => void;
	}

	let { favoriteTracks, favoritesAnnouncement, managementFeedback,
		favoritesExportStatusShown, favoritesExportFallbackShown, confirmClear,
		shelfSection = $bindable(null), shelfHeading = $bindable(null), clearButton = $bindable(null),
		openFavoriteMenu, selectTrack, moveFavoriteLabel, moveFavorite, unstarFromShelf,
		queueFavorites, copyFavoritesText, downloadFavoritesM3U, requestClearFavorites,
		cancelClearFavorites, confirmClearFavorites }: Props = $props();
</script>

		<section data-testid="favorites-section" aria-label="Favorites" bind:this={shelfSection}>
			<h2 tabindex="-1" bind:this={shelfHeading}>Favorites</h2>
			<p data-testid="favorites-announcement" aria-live="polite">{favoritesAnnouncement}</p>
			{#if managementFeedback}
				<p role="status">{managementFeedback}</p>
			{/if}
			{#if favoriteTracks.length === 0}
				<p data-testid="favorites-empty" tabindex="-1">
					No favorites yet. Star a track to pin it here.
				</p>
			{:else}
				<button class="nt-button" type="button" onclick={queueFavorites}>Queue favorites</button>
				<ul>
					{#each favoriteTracks as track, index (libraryItemKey(track))}
						<li class="nt-favorite-item" data-testid="favorite-item" data-id={track.id}
							use:contextMenu={(invoker, point) => openFavoriteMenu(track.id, invoker, point)}>
							<button
								type="button"
								class="nt-favorite-row"
								data-context-menu-longpress
								onclick={() => selectTrack(track.id, track)}
							>
								{displayTitle(track)}
								{#if displayArtist(track)}
									<span> — {displayArtist(track)}</span>
								{/if}
							</button>
							<button
								type="button"
								class="nt-favorite-move"
								aria-label={moveFavoriteLabel(track, 'up')}
								disabled={index === 0}
								onclick={(event) => void moveFavorite(track.id, 'up', event)}
							>
								Up
							</button>
							<button
								type="button"
								class="nt-favorite-move"
								aria-label={moveFavoriteLabel(track, 'down')}
								disabled={index === favoriteTracks.length - 1}
								onclick={(event) => void moveFavorite(track.id, 'down', event)}
							>
								Down
							</button>
							<button
								type="button"
								class="nt-shelf-unstar"
								data-shelf-unstar
								onclick={() => unstarFromShelf(track.id)}
							>
								<span aria-hidden="true">★</span>
								Unstar {displayTitle(track)}
							</button>
							<button type="button" class="nt-favorite-more" data-testid="favorite-more"
								aria-label={`More actions for ${displayTitle(track)}`} aria-haspopup="menu"
								onclick={(event) => openFavoriteMenu(track.id, event.currentTarget)}
								><svg aria-hidden="true" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="1" /><circle cx="19" cy="12" r="1" /><circle cx="5" cy="12" r="1" /></svg></button>
						</li>
					{/each}
				</ul>
				<div class="nt-favorites-export">
					<div class="nt-favorites-export-actions">
						<button
							class="nt-button"
							type="button"
							data-testid="favorites-export-copy"
							aria-describedby="favorites-export-status"
							onclick={() => void copyFavoritesText()}>Copy favorites as text</button
						>
						<button
							class="nt-button"
							type="button"
							data-testid="favorites-export-download"
							aria-describedby="favorites-export-status"
							onclick={downloadFavoritesM3U}>Download favorites (.m3u)</button
						>
					</div>
					<p id="favorites-export-status" data-testid="favorites-export-status" aria-live="polite">
						{favoritesExportStatusShown}
					</p>
					{#if favoritesExportFallbackShown !== null}
						<label for="favorites-export-fallback-field">Favorites as text</label>
						<textarea
							id="favorites-export-fallback-field"
							data-testid="favorites-export-fallback"
							readonly
							rows="4"
							value={favoritesExportFallbackShown}
							aria-describedby="favorites-export-instructions"
						></textarea>
						<p id="favorites-export-instructions" data-testid="favorites-export-instructions">
							Copy manually: focus the field, select all (Ctrl+A or Cmd+A), then copy (Ctrl+C or
							Cmd+C). On touch screens, long-press the text, select all and choose Copy.
						</p>
					{/if}
				</div>
			{/if}
			<div class="nt-shelf-clear">
				<button
					class="nt-button nt-button--ghost"
					type="button"
					bind:this={clearButton}
					onclick={requestClearFavorites}
				>
					Clear local favorites
				</button>
				{#if confirmClear}
					<p>Clear all local favorites? This cannot be undone.</p>
					<div class="nt-shelf-confirm">
						<button
							class="nt-button nt-button--ghost"
							type="button"
							onclick={cancelClearFavorites}
						>
							Cancel
						</button>
						<button class="nt-button" type="button" onclick={confirmClearFavorites}>
							Yes, clear local favorites
						</button>
					</div>
				{/if}
			</div>
		</section>

<style>
	.nt-favorite-more { font: inherit; padding: var(--nt-space-2); }
	[data-context-menu-longpress] { -webkit-touch-callout: none; user-select: none; }
	@media (pointer: coarse) {
		.nt-favorite-more, .nt-favorite-row, .nt-favorite-move, .nt-shelf-unstar { min-width: 48px; min-height: 48px; }
	}
	.nt-favorite-item {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		padding: 0;
	}
	.nt-favorite-row {
		display: block;
		flex: 1 1 auto;
		min-width: 0;
		text-align: left;
		background: none;
		border: 0;
		padding: 0.5rem 0.75rem;
		cursor: pointer;
		font: inherit;
		color: inherit;
	}
	.nt-favorite-move {
		flex: 0 0 auto;
		background: none;
		border: 0;
		padding: 0.5rem 0.5rem;
		cursor: pointer;
		font: inherit;
		color: inherit;
	}
	.nt-favorite-move:disabled {
		cursor: default;
		opacity: 0.4;
	}
	.nt-shelf-unstar {
		flex: 0 0 auto;
		max-width: 100%;
		background: none;
		border: 0;
		padding: 0.5rem 0.75rem;
		cursor: pointer;
		font: inherit;
		color: inherit;
		text-align: left;
	}
	.nt-shelf-clear {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: var(--nt-space-2);
	}
	.nt-shelf-confirm {
		display: flex;
		flex-wrap: wrap;
		gap: var(--nt-space-2) var(--nt-space-3);
	}
	.nt-favorites-export {
		margin-block: var(--nt-space-3);
	}
	.nt-favorites-export-actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--nt-space-2) var(--nt-space-3);
	}
	.nt-favorites-export-actions .nt-button {
		min-height: 2.75rem;
		max-width: 100%;
	}
	.nt-favorites-export p {
		margin-block: var(--nt-space-2);
	}
	.nt-favorites-export textarea {
		display: block;
		width: 100%;
		min-width: 0;
		max-width: 100%;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}
</style>
