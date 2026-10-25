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
		saveFavorites,
		selectFavoriteItems
	} from '#lib/library/favorites.js';
	import { moveFavoriteId, type FavoriteMoveDirection } from '#lib/library/favorites-order.js';
	import { createLibraryRefresher } from '#lib/library/refresh.js';
	import {
		displayArtist,
		displayTitle,
		orderLibraryItems,
		type LibraryOrder
	} from '#lib/library/order.js';
	import { filterLibraryItems } from '#lib/library/filter.js';
	import { resolveStreamUrl } from '#lib/library/stream-url.js';
	import {
		appendResultLabel,
		appendSelection,
		clearSelection,
		createSelection,
		hiddenSelectedCount,
		pruneSelection,
		selectionCountLabel,
		toggleSelected
	} from '#lib/library/selection.js';
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
		type QueueState,
		type Track
	} from '#lib/player/queue.js';

	let items: LibraryItem[] = $state([]);
	let order: LibraryOrder = $state('original');
	const orderedItems = $derived(orderLibraryItems(items, order));
	/**
	 * Transient find-in-loaded-tracks query. Presentation-only: it filters
	 * the already-ordered rows below without fetching, queueing, starring
	 * or persisting anything. Cleared only at the account/auth boundary;
	 * same-account refreshes (including failed ones) preserve it.
	 */
	let findQuery = $state('');
	let staleAt: string | null = $state(null);
	let loading = $state(true);
	let loadError: string | null = $state(null);
	let libraryUnavailable = $state(false);
	let requiresSignIn = $state(false);
	let queue: QueueState = $state(createQueue([]));
	let favorites: string[] = $state([]);
	let favoritesInitialized = false;
	let favoritesAnnouncement = $state('');
	/**
	 * Transient Favorites-only view over the loaded list. Plain component
	 * state: never persisted, never fetched, reset on reload (fresh state)
	 * and at the auth boundary. Filters after ordering via the pure
	 * selectFavoriteItems projection; the queue never sees it.
	 */
	let view: 'all' | 'favorites' = $state('all');
	const displayedItems = $derived(
		view === 'all' ? orderedItems : selectFavoriteItems(orderedItems, favorites)
	);
	const visibleItems = $derived(filterLibraryItems(displayedItems, findQuery));
	let viewSelect: HTMLSelectElement | null = $state(null);
	let confirmClear = $state(false);
	let managementFeedback = $state('');
	let shelfSection: HTMLElement | null = $state(null);
	let shelfHeading: HTMLHeadingElement | null = $state(null);
	let clearButton: HTMLButtonElement | null = $state(null);
	/**
	 * Explicit subset selection: page-local ordered stable IDs in checkbox
	 * activation order. Never persisted, never fetched, never shared across
	 * tabs; purged only at auth/account boundaries, pruned only against a
	 * successful snapshot. The last append outcome stays until the next
	 * append or a boundary purge.
	 */
	let selectedIds: string[] = $state(createSelection());
	const hiddenSelectionCount = $derived(
		hiddenSelectedCount(
			selectedIds,
			visibleItems.map((track) => track.id)
		)
	);
	let appendFeedback: string | null = $state(null);
	let queueAnnouncement = $state('');
	let queueAnnouncementVersion = $state(0);
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
	 * A branch swap (error/empty) unmounts the panel, so completion restores
	 * focus to the remounted input in that case — and never otherwise.
	 * Background refreshes keep the panel mounted, making this a no-op there.
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
			if (state.loadError && state.items.length === 0) {
				queueFilterQuery = '';
				findQuery = '';
				view = 'all';
				confirmClear = false;
				managementFeedback = '';
			}
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
			// The pending subset is per-account: an access refusal or auth
			// transition that empties the library purges it, so the next
			// account never inherits the previous choices. A successful
			// same-account refresh prunes IDs the new snapshot no longer
			// carries, preserving activation order. Transient failures keep
			// their items through the stale fallback, so prune is a no-op
			// there; failures without a snapshot have no rows selected.
			if (!state.loading) {
				if ((state.loadError && state.items.length === 0) || state.libraryUnavailable) {
					if (selectedIds.length > 0 || appendFeedback !== null) purgeSelection();
				} else if (!state.loadError) {
					selectedIds = pruneSelection(
						selectedIds,
						state.items.map((track) => track.id)
					);
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
			// Same-account refresh completion: the panel may have remounted
			// (branch swap), so hand focus back to the filter input the user
			// was typing in. Background refreshes keep the panel mounted, so
			// the restore is a no-op there. Any other focus placement (or an
			// empty query) is left alone. The page-script unit harness runs
			// this handler with neither DOM nor svelte bindings, so the
			// restore is skipped there.
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
	// One-action bulk queue: the same displayed ordered collection the rows
	// render, so sorting or filtering changes what an activation appends. The
	// count names only missing IDs, never the view's total.
	const bulkMissing = $derived(missingDisplayedTracks(visibleItems, queuedIds));
	const bulkDisabled = $derived(
		libraryUnavailable || visibleItems.length === 0 || bulkMissing.length === 0
	);
	const bulkAllQueued = $derived(
		!libraryUnavailable && visibleItems.length > 0 && bulkMissing.length === 0
	);
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

	function missingDisplayedTracks(
		displayed: readonly LibraryItem[],
		existingIds: ReadonlySet<string>
	): LibraryItem[] {
		const knownIds = new Set(existingIds);
		return displayed.filter((track) => {
			if (knownIds.has(track.id)) return false;
			knownIds.add(track.id);
			return true;
		});
	}

	function queuedTrackFor(item: LibraryItem): Track {
		// No stream URL until the streaming slice: playback wiring is out of
		// scope, so the queue carries library display fields only.
		return {
			id: item.id,
			title: displayTitle(item),
			artist: displayArtist(item) ?? '',
			streamUrl: ''
		};
	}

	function selectTrack(id: string): void {
		const item = items.find((track) => track.id === id);
		if (!item) return;
		// addTrack dedupes, so re-selecting a queued row is a no-op.
		replaceQueue(addTrack(queue, queuedTrackFor(item)));
	}

	/**
	 * Append every displayed-but-missing track in displayed order. The
	 * deduplicated snapshot preserves addTrack's append/current-index semantics
	 * in one immutable queue update, so the existing arrangement, current
	 * selection and stored metadata survive; repeating the activation adds
	 * nothing. The count names only the missing IDs this activation appended —
	 * metadata-only, never audible playback.
	 */
	function addDisplayedTracksToQueue(): void {
		const missing = bulkMissing;
		if (missing.length === 0) return;
		replaceQueue({
			tracks: [...queue.tracks, ...missing.map(queuedTrackFor)],
			currentIndex: queue.currentIndex < 0 ? 0 : queue.currentIndex
		});
		queueAnnouncement = `Added ${missing.length} ${missing.length === 1 ? 'track' : 'tracks'} to the queue.`;
	}

	function queueFavorites(): void {
		if (favoriteTracks.length === 0) return;
		const before = queue.tracks.length;
		// Use the shelf's resolved star order and the same deduping path as
		// individual taps. No persistence, playback, or account lookup here.
		for (const track of favoriteTracks) selectTrack(track.id);
		const added = queue.tracks.length - before;
		queueAnnouncement = `Queued ${added} ${added === 1 ? 'favorite' : 'favorites'}, ${favoriteTracks.length - added} already queued.`;
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

	function removeQueuedTrack(index: number): void {
		const removed = queue.tracks[index];
		if (removed === undefined) return;
		const next = removeTrack(queue, index);
		// Announce real removals only, including distinct tracks with equal
		// titles: the version refreshes the content inside the stable region.
		if (next === queue) return;
		const title = removed.title?.trim() ? removed.title : removed.id;
		queue = next;
		queueAnnouncement = `${title} removed from the queue.`;
		queueAnnouncementVersion += 1;
	}

	function clearQueuedTracks(): void {
		const next = clearQueue(queue);
		// Announce real clears only: a no-op clear must not chatter the live
		// region.
		if (next === queue) return;
		queue = next;
		queueAnnouncement = 'Queue cleared.';
		queueAnnouncementVersion += 1;
	}

	/**
	 * Metadata-only direct selection: apply the validated selectTrack
	 * transition on the ORIGINAL queue coordinate, preserving every track id
	 * and arrangement. Never transport, audio, storage, provider or stream
	 * language. Reselecting the current row is an observable no-op that keeps
	 * the existing feedback; stale or invalid coordinates select nothing.
	 */
	function selectQueuedTrack(index: number): void {
		if (!Number.isInteger(index) || index < 0 || index >= queue.tracks.length) return;
		if (index === queue.currentIndex) return;
		const target = queue.tracks[index];
		if (target === undefined) return;
		const selected = selectQueueIndex(queue, index);
		if (selected === queue) return;
		queue = selected;
		const title = target.title?.trim() ? target.title : target.id;
		queueAnnouncement = `Selected ${title}`;
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
		favoritesAnnouncement = '';
		saveFavorites(browserStorage('session'), browserStorage('local'), next);
		// Favorites-only unmounts an unstarred row, including its focused button.
		// All-tracks keeps that button mounted and must leave focus alone.
		if (view === 'favorites' && !next.includes(id)) focusLibraryView();
	}

	function focusLibraryView(): void {
		void tick().then(() => viewSelect?.focus({ preventScroll: true }));
	}

	function showAllTracks(): void {
		view = 'all';
		focusLibraryView();
	}

	function toggleQueuedFavorite(id: string): void {
		const track = queue.tracks.find((track) => track.id === id);
		if (!track) return;
		const wasFavorite = favorites.includes(id);
		toggleFavorite(id);
		queueAnnouncement = `${wasFavorite ? 'Unstarred' : 'Starred'} ${track.title}`;
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
				// Closest-ancestor lookup, never id interpolation: track ids
				// are opaque server strings, and any quoting scheme risks an
				// invalid selector (a trailing backslash throws inside this
				// voided handler). The activated button is always inside its
				// own favorite item.
				const item = button.closest('[data-testid="favorite-item"]');
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

	function clearFind(): void {
		findQuery = '';
		if (typeof document !== 'undefined') {
			document.getElementById('library-find')?.focus({ preventScroll: true });
		}
	}

	/**
	 * Shelf unstar: remove one id from the full visit state (hidden ids ride
	 * along untouched) and persist the whole set through the existing saver.
	 * Queue, ordering and refresh are untouched. Focus moves to the next
	 * surviving shelf unstar control, or the shelf heading when none remains.
	 */
	function unstarFromShelf(id: string): void {
		const removedIndex = favoriteTracks.findIndex((item) => item.id === id);
		const track = items.find((item) => item.id === id);
		const title = track ? displayTitle(track) : 'track';
		favorites = favorites.filter((favorite) => favorite !== id);
		favoritesAnnouncement = '';
		saveFavorites(browserStorage('session'), browserStorage('local'), favorites);
		managementFeedback = `Removed ${title} from favorites.`;
		void tick().then(() => {
			const controls = shelfSection?.querySelectorAll<HTMLButtonElement>('[data-shelf-unstar]');
			const next = controls?.[Math.max(0, Math.min(removedIndex, controls.length - 1))];
			if (next) next.focus({ preventScroll: true });
			else shelfHeading?.focus({ preventScroll: true });
		});
	}

	function requestClearFavorites(): void {
		confirmClear = true;
	}

	/** Cancel is a pure UI reset: no save, queue and favorites untouched. */
	function cancelClearFavorites(): void {
		confirmClear = false;
		managementFeedback = `Clear cancelled. ${trackCountLabel(favorites.length)} kept.`;
		void tick().then(() => clearButton?.focus({ preventScroll: true }));
	}

	/**
	 * Confirmed clear: persist the entire empty scoped set (including ids
	 * absent from this library) through the existing saver. Never touches
	 * the auth context, other accounts, the queue or the network.
	 */
	function confirmClearFavorites(): void {
		favorites = [];
		favoritesAnnouncement = '';
		saveFavorites(browserStorage('session'), browserStorage('local'), []);
		confirmClear = false;
		managementFeedback = 'Cleared local favorites.';
		void tick().then(() => clearButton?.focus({ preventScroll: true }));
	}

	function toggleSelectedId(id: string): void {
		selectedIds = toggleSelected(selectedIds, id);
	}

	function clearSelectedIds(): void {
		selectedIds = clearSelection(selectedIds);
	}

	function appendSelectedIds(): void {
		const result = appendSelection(queue, selectedIds, items);
		replaceQueue(result.queue);
		appendFeedback = appendResultLabel(result.added, result.skipped);
	}

	function purgeSelection(): void {
		selectedIds = createSelection();
		appendFeedback = null;
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
			findQuery = '';
			view = 'all';
			confirmClear = false;
			managementFeedback = '';
			purgeSelection();
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
		findQuery = '';
		view = 'all';
		confirmClear = false;
		managementFeedback = '';
		purgeSelection();
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

	{#if loading && items.length === 0}
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
		{#if loading}
			<p role="status">Loading your library…</p>
		{/if}
		{#if staleAt}
			<p data-testid="library-stale-badge" role="status">{staleLabel(staleAt)}</p>
		{/if}

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
							<button
								type="button"
								class="nt-shelf-unstar"
								data-shelf-unstar
								onclick={() => unstarFromShelf(track.id)}
							>
								<span aria-hidden="true">★</span>
								Unstar {displayTitle(track)}
							</button>
						</li>
					{/each}
				</ul>
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

		<div class="nt-bulk-queue">
			<button
				class="nt-button nt-bulk-queue-action"
				type="button"
				disabled={bulkDisabled}
				aria-describedby={bulkAllQueued ? 'bulk-queue-hint' : undefined}
				onclick={addDisplayedTracksToQueue}
			>
				Add displayed tracks to queue
			</button>
			{#if bulkAllQueued}
				<p id="bulk-queue-hint">All displayed tracks are already in the queue.</p>
			{/if}
		</div>

		{#if libraryUnavailable}
			<p role="status">Library listing is not available yet.</p>
			<p><a href="/library/import">Import an NZB</a></p>
		{:else if items.length === 0}
			<p role="status">Your library is empty. <a href="/library/import">Import an NZB</a> to get started.</p>
		{:else}
			<p data-testid="library-track-count">{trackCountLabel(items.length)}</p>
			<div class="nt-library-find">
				<label for="library-find">Find in loaded tracks</label>
				<input
					id="library-find"
					type="search"
					bind:value={findQuery}
					aria-describedby="library-find-hint"
				/>
				<p id="library-find-hint">
					Searches only the tracks already loaded here, not the remote catalog.
				</p>
				{#if findQuery !== ''}
					<button class="nt-button" type="button" onclick={clearFind}>
						Clear search
					</button>
				{/if}
			</div>
			<div class="nt-library-order">
				<label for="library-order">Order tracks</label>
				<select id="library-order" bind:value={order} aria-describedby="library-order-hint">
					<option value="original">Original order</option>
					<option value="title">Title A–Z</option>
					<option value="artist">Artist A–Z</option>
					<option value="title-desc">Title Z–A</option>
					<option value="artist-desc">Artist Z–A</option>
				</select>
				<span id="library-order-hint">Changes this list only, not your queue.</span>
			</div>
			<div class="nt-library-view">
				<label for="library-view">Show</label>
				<select id="library-view" bind:this={viewSelect} bind:value={view}>
					<option value="all">All tracks</option>
					<option value="favorites">Favorites only</option>
				</select>
			</div>
			<p data-testid="library-find-count">
				Showing {visibleItems.length} of {items.length}
				{visibleItems.length === 1 ? 'track' : 'tracks'}
			</p>
			{#if view === 'favorites' && displayedItems.length === 0}
				<p>
					No favorites in this list.
					<button type="button" onclick={showAllTracks}>Show all tracks</button>
				</p>
			{/if}
			<div class="nt-selection-bar">
				<p data-testid="selection-count">{selectionCountLabel(selectedIds.length)}</p>
				<button class="nt-button" type="button" onclick={() => clearSelectedIds()}>
					Clear selection
				</button>
				<button
					class="nt-button"
					type="button"
					disabled={selectedIds.length === 0}
					onclick={() => appendSelectedIds()}
				>
					Append selected
				</button>
				{#if appendFeedback}
					<p data-testid="append-result" role="status">{appendFeedback}</p>
				{/if}
				{#if hiddenSelectionCount > 0}
					<p data-testid="selection-hidden-count">
						{hiddenSelectionCount} selected {hiddenSelectionCount === 1 ? 'track is' : 'tracks are'} hidden by this {view === 'all' ? 'find' : 'view'}.
					</p>
				{/if}
			</div>
			{#if displayedItems.length > 0 && visibleItems.length === 0}
				<p>No matches in your loaded tracks.</p>
				<button class="nt-button" type="button" onclick={clearFind}>
					Clear search
				</button>
			{:else}
			<ul data-testid="library-list">
				{#each visibleItems as track (track.id)}
					{@const queued = queuedIds.has(track.id)}
					<li class="nt-library-item" data-testid="library-item" data-id={track.id}>
						<input
							class="nt-library-select"
							type="checkbox"
							aria-label={`Select ${displayTitle(track)}`}
							checked={selectedIds.includes(track.id)}
							onclick={(event) => event.stopPropagation()}
							onchange={() => toggleSelectedId(track.id)}
						/>
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
			{/if}
			<!--
				The queue panel stays mounted across background refreshes (its
				rows remain readable while the new list loads), but its four
				live statuses suspend while loading so the inline loading
				indicator keeps the single-status transient the ordering specs
				pin with an unfiltered role query. Content, controls and
				focus are untouched; the roles return with the fresh list.
			-->
			<PlayerQueue
				state={queue}
				announcement={queueAnnouncement}
				announcementVersion={queueAnnouncementVersion}
				{favoriteIds}
				ontogglefavorite={toggleQueuedFavorite}
				bind:filterQuery={queueFilterQuery}
				liveRegions={!loading}
				onnext={() => replaceQueue(nextTrack(queue))}
				onshuffle={() => replaceQueue(shuffleUpcoming(queue))}
				onmove={moveQueuedTrack}
				onplaynext={(index) => replaceQueue(playNext(queue, index))}
				onremove={removeQueuedTrack}
				onclear={clearQueuedTracks}
				onselect={selectQueuedTrack}
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
			{#if currentTrack}
				<p class="nt-player-identity" data-testid="library-now-playing">
					{displayTitle(currentTrack)}{#if displayArtist(currentTrack)}{' — '}{displayArtist(currentTrack)}{/if}
				</p>
			{/if}
			<audio data-testid="audio-player" controls src={streamUrl}></audio>
		</section>
	{/if}
</main>

<style>
	.nt-library-find {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--nt-space-2) var(--nt-space-3);
	}
	.nt-library-find input {
		box-sizing: border-box;
		width: min(100%, 20rem);
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}
	.nt-library-find p {
		flex-basis: 100%;
		margin: 0;
		color: var(--nt-fg-muted);
	}
	.nt-player-identity {
		overflow-wrap: anywhere;
	}
	audio {
		max-width: 100%;
	}
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
	.nt-bulk-queue {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--nt-space-2) var(--nt-space-3);
		margin-block: var(--nt-space-2);
	}
	.nt-bulk-queue-action {
		display: inline-flex;
		align-items: center;
		min-height: 2.75rem;
	}
	.nt-bulk-queue p {
		margin: 0;
		color: var(--nt-fg-muted);
	}
	.nt-library-view {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--nt-space-2) var(--nt-space-3);
	}
	.nt-library-view select {
		max-width: 100%;
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
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
	.nt-selection-bar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--nt-space-2) var(--nt-space-3);
	}
	.nt-selection-bar p {
		margin: 0;
	}
	.nt-selection-bar [data-testid='append-result'] {
		flex-basis: 100%;
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
