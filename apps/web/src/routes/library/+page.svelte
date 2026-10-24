<script lang="ts">
	import { onMount, tick } from 'svelte';
	import {
		browserStorage,
		clearSnapshot,
		invalidateLibraryCache,
		LIBRARY_AUTH_TRANSITION_KEY,
		staleLabel,
		type LibraryItem
	} from '#lib/library/cache.js';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import {
		clearFavoritesAtAuthBoundary,
		loadFavorites,
		saveFavorites
	} from '#lib/library/favorites.js';
	import { moveFavoriteId, type FavoriteMoveDirection } from '#lib/library/favorites-order.js';
	import { createLibraryRefresher } from '#lib/library/refresh.js';
	import {
		displayArtist,
		displayTitle,
		orderLibraryItems,
		type LibraryOrder
	} from '#lib/library/order.js';
	import { resolveStreamUrl } from '#lib/library/stream-url.js';
	import { clearSignedInEmail } from '#lib/signed-in-email.js';
	import PlayerQueue from '#lib/player/PlayerQueue.svelte';
	import {
		addTrack,
		clearQueue,
		clearUpcoming,
		createQueue,
		currentTrack as queueCurrentTrack,
		keepCurrentAndUpcoming,
		moveTrack,
		nextTrack,
		playNext,
		removeTrack,
		selectTrack as selectQueueIndex,
		shuffleUpcoming,
		type QueueState
	} from '#lib/player/queue.js';

	let items: LibraryItem[] = $state([]);
	let order: LibraryOrder = $state('original');
	const orderedItems = $derived(orderLibraryItems(items, order));
	let staleAt: string | null = $state(null);
	let loading = $state(true);
	let loadError: string | null = $state(null);
	let libraryUnavailable = $state(false);
	let requiresSignIn = $state(false);
	let queue: QueueState = $state(createQueue([]));
	let favorites: string[] = $state([]);
	let favoritesInitialized = false;
	let favoritesAnnouncement = $state('');
	let queueAnnouncement = $state('');
	let refreshButton: HTMLButtonElement | null = $state(null);
	/**
	 * Transient queue-only filter text, owned here (not in the panel) so
	 * it survives the panel unmounting/remounting across library refresh
	 * branch swaps. Cleared only at the account/auth boundary below;
	 * same-account refreshes preserve it. Never persisted or fetched.
	 */
	let queueFilterQuery = $state('');
	/**
	 * True only when a refresh starts while the filter input holds focus.
	 * The loading branch unmounts the panel, so completion restores focus
	 * to the remounted input in that case — and never otherwise.
	 */
	let restoreFilterFocus = false;

	const refresher = createLibraryRefresher({
		sessionStorage: () => browserStorage('session'),
		invalidateLibraryCache: () => {
			favorites = [];
			favoritesInitialized = false;
			favoritesAnnouncement = '';
			invalidateLibraryCache();
		},
		onStateChange: (state) => {
			items = state.items;
			staleAt = state.staleAt;
			loading = state.loading;
			loadError = state.loadError;
			libraryUnavailable = state.libraryUnavailable;
			requiresSignIn = state.requiresSignIn;
			// An access refusal that empties the library unmounts the queue
			// panel (alert branch); the route-owned filter text must go with
			// it so the next account never inherits the previous query.
			// Successful same-account refreshes keep items and keep the query.
			if (state.loadError && state.items.length === 0) queueFilterQuery = '';
			// The queue is per-account: drop tracks the new library no longer
			// contains, so a 401/403/404 or auth transition cannot leak the
			// previous account's queue into the next session's Retry. The
			// current track stays current when it survives the refresh.
			if (queue.tracks.length > 0) {
				const ids = new Set(state.items.map((track) => track.id));
				const kept = queue.tracks.filter((track) => ids.has(track.id));
				if (kept.length !== queue.tracks.length) {
					const currentId = queueCurrentTrack(queue)?.id ?? null;
					const rebuilt = createQueue(kept);
					const index =
						currentId === null ? -1 : kept.findIndex((track) => track.id === currentId);
					replaceQueue(index > 0 ? selectQueueIndex(rebuilt, index) : rebuilt);
				}
			}
			// Bootstrap once after an authenticated load has minted the context.
			// Memory stays authoritative for this visit, even if persistence
			// fails; Refresh must not overwrite pending edits with stale storage.
			// Auth transitions and access refusals reset both memory and bootstrap.
			if (!state.loading && !state.loadError && !state.libraryUnavailable && !favoritesInitialized) {
				favorites = loadFavorites(browserStorage('session'), browserStorage('local'));
				favoritesInitialized = true;
			}
			// Same-account refresh completion: the panel remounted, so hand
			// focus back to the filter input the user was typing in. Any
			// other focus placement (or an empty query) is left alone. The
			// page-script unit harness runs this handler with neither DOM
			// nor svelte bindings, so the restore is skipped there.
			if (!state.loading && restoreFilterFocus) {
				restoreFilterFocus = false;
				if (
					queueFilterQuery !== '' &&
					typeof document !== 'undefined' &&
					typeof tick === 'function'
				) {
					void tick().then(() =>
						document.getElementById('queue-filter-input')?.focus({ preventScroll: true })
					);
				}
			}
		}
	});

	// e2e contract hooks (e2e/helpers/contract.ts TESTIDS) owned by sibling
	// slices: usage metering and plan-limit enforcement land with the
	// venture slices that own them. The placeholder below keeps its hook
	// rendered until then.
	const overLimit = false;
	// Queue metadata alone is not playable media: the player renders only
	// when resolveStreamUrl finds a real (non-blank) stream URL.
	const currentTrack = $derived(queueCurrentTrack(queue));
	const queuedIds = $derived(new Set(queue.tracks.map((track) => track.id)));
	const streamUrl = $derived(resolveStreamUrl(currentTrack));
	const favoriteIds = $derived(new Set(favorites));
	// Favorites render in star order; ids absent from the current library
	// stay stored but unlisted (there is no title to show for them).
	const favoriteTracks = $derived(
		favorites
			.map((id) => items.find((track) => track.id === id) ?? null)
			.filter((track): track is LibraryItem => track !== null)
	);

	function trackCountLabel(count: number): string {
		return `${count} ${count === 1 ? 'track' : 'tracks'}`;
	}

	function selectTrack(id: string): void {
		const item = items.find((track) => track.id === id);
		if (!item) return;
		// No stream URL until the streaming slice: playback wiring is out of
		// scope, so the queue carries library display fields only. addTrack
		// dedupes, so re-selecting a queued row is a no-op.
		replaceQueue(
			addTrack(queue, {
				id: item.id,
				title: displayTitle(item),
				artist: displayArtist(item) ?? '',
				streamUrl: ''
			})
		);
	}

	/**
	 * Swap the queue for any change that is not a move. The move
	 * announcement names a track and its position, so every other edit
	 * invalidates it; clearing it here also keeps a previous account's
	 * track title from lingering across the auth-boundary clears below.
	 */
	function replaceQueue(next: QueueState): void {
		queue = next;
		queueAnnouncement = '';
	}

	function moveQueuedTrack(from: number, to: number): void {
		const moved = queue.tracks[from];
		if (moved === undefined) return;
		const reordered = moveTrack(queue, from, to);
		// Announce real moves only: a no-op reorder must not chatter the
		// live region.
		if (reordered === queue) return;
		queue = reordered;
		queueAnnouncement = `${moved.title} moved to ${to + 1} of ${reordered.tracks.length}`;
	}

	function trimPreviousTracks(): void {
		const trimmed = keepCurrentAndUpcoming(queue);
		// Announce real trims only: a no-op trim must not chatter the live
		// region. Rows before the selection are only unselected, never proof
		// of listening, so the copy never calls them played.
		if (trimmed === queue) return;
		const removed = queue.tracks.length - trimmed.tracks.length;
		queue = trimmed;
		queueAnnouncement = `Removed ${removed} ${removed === 1 ? 'track' : 'tracks'} before the current track.`;
	}

	function trimUpcomingTracks(): void {
		const trimmed = clearUpcoming(queue);
		// Announce real trims only: a no-op trim must not chatter the live
		// region.
		if (trimmed === queue) return;
		const removed = queue.tracks.length - trimmed.tracks.length;
		queue = trimmed;
		queueAnnouncement = `Removed ${removed} upcoming ${removed === 1 ? 'track' : 'tracks'}.`;
	}

	function toggleFavorite(id: string): void {
		// Toggle visible state, not the last successful write. The next write
		// persists the full set, including edits made while storage failed.
		const next = favorites.includes(id)
			? favorites.filter((favorite) => favorite !== id)
			: [...favorites, id];
		favorites = next;
		saveFavorites(browserStorage('session'), browserStorage('local'), next);
	}

	function starLabel(track: LibraryItem): string {
		const title = displayTitle(track);
		return favoriteIds.has(track.id) ? `Unstar ${title}` : `Star ${title}`;
	}

	/**
	 * Move a visible favorite one slot up or down within the complete
	 * stored order. No-ops (boundary activation, stale id) return the same
	 * array and change nothing: no save, no announcement, no focus shift.
	 * Real moves persist the full list through the existing seam, announce
	 * the new visible position politely, and keep keyboard focus on the
	 * moved track's controls. Never touches the queue, the library view,
	 * stars, refresh or the network.
	 */
	async function moveFavorite(
		id: string,
		direction: FavoriteMoveDirection,
		event: Event
	): Promise<void> {
		const button = event.currentTarget;
		const visibleIds = favoriteTracks.map((track) => track.id);
		const next = moveFavoriteId(favorites, visibleIds, id, direction);
		if (next === favorites) return;
		favorites = [...next];
		saveFavorites(browserStorage('session'), browserStorage('local'), favorites);
		const visible = new Set(visibleIds);
		const position = next.filter((storedId) => visible.has(storedId)).indexOf(id) + 1;
		const track = items.find((candidate) => candidate.id === id);
		favoritesAnnouncement = `${track ? displayTitle(track) : id} moved to ${position} of ${visibleIds.length}`;
		await tick();
		// The keyed list preserves nodes, so an enabled button keeps focus
		// on its own; a move onto a boundary disables the activated button
		// and drops focus to the body, so land on the surviving move
		// control of the same track instead. Runs only on explicit
		// activation, so background refreshes cannot steal focus.
		if (button instanceof HTMLButtonElement && typeof document !== 'undefined') {
			if (!button.disabled && button.isConnected) {
				button.focus({ preventScroll: true });
			} else {
				const item = document.querySelector(
					`[data-testid="favorite-item"][data-id="${id.replace(/"/g, '\\"')}"]`
				);
				const fallback =
					item?.querySelector<HTMLButtonElement>(
						'button[aria-label^="Move favorite"]:not([disabled])'
					) ?? item?.querySelector<HTMLButtonElement>('button:not([disabled])');
				fallback?.focus({ preventScroll: true });
			}
		}
	}

	function moveFavoriteLabel(track: LibraryItem, direction: FavoriteMoveDirection): string {
		return `Move favorite ${displayTitle(track)} ${direction}`;
	}

	function refresh(): Promise<void> {
		restoreFilterFocus =
			typeof document !== 'undefined' &&
			document.activeElement?.id === 'queue-filter-input';
		return refresher.refresh();
	}

	/**
	 * Button-activated refresh with deterministic focus: the loading branch
	 * unmounts this button, dropping focus to the body. Restore it only when
	 * focus was lost that way — never when the user moved on, and never on
	 * mount/auth/transition refreshes, which do not run through this handler.
	 * Both the list-branch Refresh and the error-branch Retry bind the same
	 * ref; the branches are mutually exclusive, so the ref always points at
	 * the mounted successor (including a Refresh-to-401 landing on Retry).
	 */
	async function refreshFromButton(): Promise<void> {
		await refresh();
		await tick();
		if (document.activeElement === null || document.activeElement === document.body) {
			refreshButton?.focus({ preventScroll: true });
		}
	}

	function handleAuthTransition(event: StorageEvent): void {
		// Clear the queue and this tab's identity label at the auth boundary
		// even before the refresher's state lands; the onStateChange
		// reconcile above is the backstop for the queue.
		if (event.key === LIBRARY_AUTH_TRANSITION_KEY) {
			replaceQueue(clearQueue(queue));
			clearSignedInEmail();
			// Clear favorites storage BEFORE the refresher revokes the
			// session auth context the scoped key derives from.
			clearFavoritesAtAuthBoundary(browserStorage('session'), browserStorage('local'));
			favorites = [];
			favoritesInitialized = false;
			favoritesAnnouncement = '';
			queueFilterQuery = '';
		}
		refresher.handleAuthTransition(event);
	}

	function handleLocalAuthTransition(): void {
		replaceQueue(clearQueue(queue));
		clearSignedInEmail();
		clearFavoritesAtAuthBoundary(browserStorage('session'), browserStorage('local'));
		favorites = [];
		favoritesInitialized = false;
		favoritesAnnouncement = '';
		queueFilterQuery = '';
		refresher.handleLocalAuthTransition();
	}

	onMount(() => {
		window.addEventListener('storage', handleAuthTransition);
		const unsubscribeLocal = subscribeLocalAuthTransition(handleLocalAuthTransition);
		// Discard the old shared localStorage snapshot from earlier versions.
		clearSnapshot(browserStorage('local'));
		void refresh();
		return () => {
			refresher.dispose();
			unsubscribeLocal();
			window.removeEventListener('storage', handleAuthTransition);
		};
	});
</script>

<svelte:head>
	<title>Library — NNTune</title>
	<meta name="description" content="Your NNTune library, available even when the API is down." />
</svelte:head>

<main class="nt-wrap">
	<h1>Library</h1>

	{#if loading}
		<p role="status">Loading your library…</p>
	{:else if loadError && items.length === 0}
		<p role="alert">{loadError}</p>
		{#if requiresSignIn}
			<a class="nt-button" href="/login">Sign in</a>
		{/if}
		<button
			class="nt-button"
			type="button"
			bind:this={refreshButton}
			onclick={() => void refreshFromButton()}
		>
			Retry
		</button>
	{:else}
		{#if staleAt}
			<p data-testid="library-stale-badge" role="status">{staleLabel(staleAt)}</p>
		{/if}

		<section data-testid="favorites-section" aria-label="Favorites">
			<h2>Favorites</h2>
			<p data-testid="favorites-announcement" aria-live="polite">{favoritesAnnouncement}</p>
			{#if favoriteTracks.length === 0}
				<p data-testid="favorites-empty">No favorites yet. Star a track to pin it here.</p>
			{:else}
				<ul>
					{#each favoriteTracks as track, index (track.id)}
						<li class="nt-favorite-item" data-testid="favorite-item" data-id={track.id}>
							<button
								type="button"
								class="nt-favorite-row"
								onclick={() => selectTrack(track.id)}
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
						</li>
					{/each}
				</ul>
			{/if}
		</section>

		{#if libraryUnavailable}
			<p role="status">Library listing is not available yet.</p>
			<p><a href="/library/import">Import an NZB</a></p>
		{:else if items.length === 0}
			<p role="status">Your library is empty. <a href="/library/import">Import an NZB</a> to get started.</p>
		{:else}
			<p data-testid="library-track-count">{trackCountLabel(items.length)}</p>
			<div class="nt-library-order">
				<label for="library-order">Order tracks</label>
				<select id="library-order" bind:value={order} aria-describedby="library-order-hint">
					<option value="original">Original order</option>
					<option value="title">Title A–Z</option>
					<option value="artist">Artist A–Z</option>
				</select>
				<span id="library-order-hint">Changes this list only, not your queue.</span>
			</div>
			<ul data-testid="library-list">
				{#each orderedItems as track (track.id)}
					{@const queued = queuedIds.has(track.id)}
					<li class="nt-library-item" data-testid="library-item" data-id={track.id}>
						<button
							type="button"
							class="nt-library-row"
							aria-label={queued
								? `${displayTitle(track)}${displayArtist(track) ? ` — ${displayArtist(track)}` : ''} — Queued`
								: undefined}
							aria-pressed={queued ? true : undefined}
							onclick={() => selectTrack(track.id)}
						>
							{displayTitle(track)}
							{#if displayArtist(track)}
								<span> — {displayArtist(track)}</span>
							{/if}
							{#if queued}
								<span class="nt-queued-badge" data-testid="library-queued-badge">Queued</span>
							{/if}
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
			<PlayerQueue
				state={queue}
				announcement={queueAnnouncement}
				bind:filterQuery={queueFilterQuery}
				onnext={() => replaceQueue(nextTrack(queue))}
				onshuffle={() => replaceQueue(shuffleUpcoming(queue))}
				onmove={moveQueuedTrack}
				onplaynext={(index) => replaceQueue(playNext(queue, index))}
				onremove={(index) => replaceQueue(removeTrack(queue, index))}
				onclear={() => replaceQueue(clearQueue(queue))}
				onkeepcurrentandupcoming={trimPreviousTracks}
				onclearupcoming={trimUpcomingTracks}
			/>
		{/if}

		<button
			class="nt-button nt-button--ghost"
			type="button"
			bind:this={refreshButton}
			onclick={() => void refreshFromButton()}
		>
			{staleAt ? 'Retry' : 'Refresh'}
		</button>
	{/if}

	<!-- Contract placeholders from main: usage-meter deliberately carries no
		role so the offline/timeout specs' zero-status assertions still hold;
		the smoke contract pins it by testid. -->
	<section aria-label="Usage">
		<h2>Usage</h2>
		<p data-testid="usage-meter">
			Usage information is not available yet.
		</p>
	</section>

	{#if overLimit}
		<p data-testid="plan-limit-notice" role="alert">
			You are over your plan limit — streaming is paused until the next cycle or an upgrade.
		</p>
	{/if}

	{#if streamUrl}
		<section aria-label="Player">
			<h2>Now playing</h2>
			<audio data-testid="audio-player" controls src={streamUrl}></audio>
		</section>
	{/if}
</main>

<style>
	.nt-library-order {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--nt-space-2) var(--nt-space-3);
	}
	.nt-library-order select {
		max-width: 100%;
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}
	.nt-library-order span {
		color: var(--nt-fg-muted);
	}
	.nt-favorite-item {
		display: flex;
		align-items: center;
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
	.nt-library-item {
		display: flex;
		align-items: center;
		padding: 0;
	}
	.nt-library-row {
		display: block;
		flex: 1 1 auto;
		width: 100%;
		text-align: left;
		background: none;
		border: 0;
		padding: 0.5rem 0.75rem;
		cursor: pointer;
		font: inherit;
		color: inherit;
	}
	.nt-library-star {
		flex: 0 0 auto;
		background: none;
		border: 0;
		padding: 0.5rem 0.75rem;
		cursor: pointer;
		font: inherit;
		color: inherit;
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
