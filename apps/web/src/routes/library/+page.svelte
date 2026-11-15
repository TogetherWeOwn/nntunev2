<script lang="ts">
	import { clipboardWriter } from '#lib/clipboard.js';
	import { createTrackListExport } from '#lib/player/trackListExport.js';
	import { onMount, tick, untrack } from 'svelte';
	import { usePlayerSession } from '#lib/player/context.js';
	import { afterNavigate } from '$app/navigation';
	import { buildShareQuery, resolveShareTarget } from '#lib/player/shareTimestamp.js';
	import {
		browserStorage,
		clearSnapshot,
		invalidateLibraryCache,
		libraryItemKey,
		LIBRARY_AUTH_TRANSITION_KEY,
		staleLabel,
		type LibraryItem
	} from '#lib/library/cache.js';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import { selectFavoriteItems } from '#lib/library/favorites.js';
	import {
		clearRecentFindsAtAuthBoundary,
		loadRecentFinds,
		recordRecentFind,
		saveRecentFinds
	} from '#lib/library/recent-finds.js';
	import { moveFavoriteId, type FavoriteMoveDirection } from '#lib/library/favorites-order.js';
	import { createLibraryRefresher } from '#lib/library/refresh.js';
	import {
		displayArtist,
		displayTitle,
		orderLibraryItems,
		type LibraryOrder
	} from '#lib/library/order.js';
	import { filterLibraryItems, type FindScope } from '#lib/library/filter.js';
	import { groupLibraryItems } from '#lib/library/group.js';
	import { libraryTabTitle } from '#lib/library/tab-title.js';
	import { resolveStreamUrl } from '#lib/library/stream-url.js';
	import {
		appendResultLabel,
		appendSelection,
		clearSelection,
		createSelection,
		hiddenSelectedCount,
		pruneSelection,
		selectAllDisplayed,
		selectRange,
		selectSelectedItems,
		selectionCountLabel,
		toggleSelected
	} from '#lib/library/selection.js';
	import { clearSignedInEmail } from '#lib/signed-in-email.js';
	import { clearMiniPlayerHint, setMiniPlayerHint } from '#lib/shell/mini-player.js';
	import ContextMenu from '#lib/interactions/ContextMenu.svelte';
	import MoreActionsGlyph from '#lib/interactions/MoreActionsGlyph.svelte';
	import { contextMenu, menuAt, unavailablePlaylistAction, type MenuRequest, type MenuAction } from '#lib/interactions/context-menu.js';
	import { startTrackDrag, endTrackDrag } from '#lib/interactions/track-drag.js';
	import SelectionExport from '#lib/library/SelectionExport.svelte';
	import {
		addTrack,
		clearQueue,
		createQueue,
		currentTrack as queueCurrentTrack,
		formatQueueM3U,
		formatQueueText,
		insertTracks,
		insertTracksNext,
		selectTrack as selectQueueIndex,
		type QueueState,
		type RepeatMode,
		type Track
	} from '#lib/player/queue.js';

	const player = usePlayerSession();

	type Density = 'comfortable' | 'compact';
	const DENSITY_KEY = 'nntune:density:v1';
	let density = $state<Density>('comfortable');

	function readStoredDensity(): Density {
		try {
			return window.localStorage.getItem(DENSITY_KEY) === 'compact' ? 'compact' : 'comfortable';
		} catch {
			return 'comfortable';
		}
	}

	function onDensityChange(event: Event): void {
		const value = (event.currentTarget as HTMLSelectElement).value;
		if (value !== 'comfortable' && value !== 'compact') return;
		density = value;
		player.setQueueDensity(value);
		try {
			window.localStorage.setItem(DENSITY_KEY, value);
		} catch {
			// Storage refused: keep the choice in memory without a banner.
		}
	}

	const repeat: RepeatMode = $derived($player.repeat);

	function onRepeatChange(event: Event): void {
		const value = (event.currentTarget as HTMLSelectElement).value;
		if (value !== 'off' && value !== 'one' && value !== 'queue') return;
		player.setRepeat(value);
	}

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
	/**
	 * Which display fields the find searches. Visit-local like the query:
	 * never persisted, reset to `'both'` at the same account/auth
	 * boundaries that clear the query. Clearing the search text keeps it.
	 */
	let findScope: FindScope = $state('both');
	/**
	 * Recent applied find queries (device-local, bounded, per auth context).
	 * Recorded only when an applied query is left (blur, Clear search, a
	 * successful refresh, navigation away) and re-applied only by an explicit
	 * chip activation: loading, restoring or purging them never fills the
	 * field, filters rows or fetches. Purged at the same auth boundaries as
	 * favorites.
	 */
	let recentFinds: string[] = $state([]);
	let recentFindsInitialized = false;
	let recentFindsAnnouncement = $state('');
	let recentFindsEmpty: HTMLParagraphElement | null = $state(null);
	let staleAt: string | null = $state(null);
	let loading = $state(true);
	let loadError: string | null = $state(null);
	let libraryUnavailable = $state(false);
	let requiresSignIn = $state(false);
	const queue: QueueState = $derived($player.queue);
	const favorites: string[] = $derived($player.favorites);
	let favoritesAnnouncement = $state('');
	// One shared polite outcome for order, view and clear actions: a new
	// outcome replaces the previous text (keyed re-announce), never queues.
	let browseAnnouncement = $state('');
	let browseAnnouncementVersion = $state(0);
	/**
	 * Explicit subset selection: page-local ordered stable IDs in checkbox
	 * activation order. Never persisted, never fetched, never shared across
	 * tabs; purged only at auth/account boundaries, pruned only against a
	 * successful snapshot. Declared here because the Selected-only
	 * projection below consumes it.
	 */
	let selectedIds: string[] = $state(createSelection());
	/**
	 * Transient view over the loaded list. Plain component state: never
	 * persisted, never fetched, reset on reload (fresh state) and at the
	 * auth boundary. Filters after ordering via the pure
	 * selectFavoriteItems/selectSelectedItems projections (loaded order as
	 * reshaped by ordering, never activation order); the queue never sees
	 * it, and switching views never mutates the selection.
	 */
	let view: 'all' | 'favorites' | 'selected' = $state('all');
	const displayedItems = $derived(
		view === 'all'
			? orderedItems
			: view === 'favorites'
				? selectFavoriteItems(orderedItems, favorites)
				: selectSelectedItems(orderedItems, selectedIds)
	);
	const visibleItems = $derived(filterLibraryItems(displayedItems, findQuery, findScope));
	// Visit-local presentation only; SSR and each account start with the flat list.
	let groupByArtist = $state(false);
	const artistGroups = $derived(groupByArtist ? groupLibraryItems(visibleItems) : []);
	// The final DOM row sequence in either presentation. Display-level
	// actions use this sequence so grouped rows and the queue agree.
	const renderedItems = $derived(
		groupByArtist ? artistGroups.flatMap((group) => group.items) : visibleItems
	);
	let groupingAnnouncement = $state('');

	function toggleArtistGrouping(): void {
		groupByArtist = !groupByArtist;
		groupingAnnouncement = groupByArtist ? 'Tracks grouped by artist.' : 'Flat track list restored.';
	}
	let viewSelect: HTMLSelectElement | null = $state(null);
	let scopeSelect: HTMLSelectElement | null = $state(null);
	let confirmClear = $state(false);
	let managementFeedback = $state('');
	let shelfSection: HTMLElement | null = $state(null);
	let shelfHeading: HTMLHeadingElement | null = $state(null);
	let clearButton: HTMLButtonElement | null = $state(null);
	const hiddenSelectionCount = $derived(
		hiddenSelectedCount(
			selectedIds,
			visibleItems.map((track) => track.id)
		)
	);
	// The last append outcome stays until the next append or a boundary purge.
	let appendFeedback: string | null = $state(null);
	/**
	 * Range anchor: the most recently activated selection checkbox, or null
	 * after Clear selection, a boundary purge or a prune that dropped it.
	 * Plain component state, never persisted; an anchor hidden by find or
	 * view degrades Shift+click to a plain toggle inside `selectRange`.
	 */
	let rangeAnchorId: string | null = null;
	// Shift state of the checkbox click whose change event is in flight.
	let extendSelection = false;
	// Bumped by every queue edit; fences late play-from-top outcomes.
	let playEpoch = 0;
	let refreshButton: HTMLButtonElement | null = $state(null);
	/**
	 * Transient queue-only filter text lives in the layout session, so
	 * refresh branch swaps and client navigation cannot discard it.
	 * Auth/access boundaries clear it; it is never persisted or fetched.
	 */
	const queueFilterQuery = $derived($player.queueFilterQuery);
	/**
	 * True only when a refresh starts while the filter input holds focus.
	 * A branch swap (error/empty) unmounts the panel, so completion restores
	 * focus to the remounted input in that case — and never otherwise.
	 * Background refreshes keep the panel mounted, making this a no-op there.
	 */
	let restoreFilterFocus = false;
	const audioEl = $derived($player.audio ?? undefined);

	// Share input remains route-owned; removing these listeners must not pause
	// the persistent source or retire the session's restart controls.
	$effect(() => {
		const element = audioEl;
		if (!element) return;
		const metadata = () => applyPendingShareSeek(element);
		element.addEventListener('loadedmetadata', metadata);
		element.addEventListener('durationchange', metadata);
		element.addEventListener('seeking', handleSeeking);
		untrack(() => applyPendingShareSeek(element));
		return () => {
			element.removeEventListener('loadedmetadata', metadata);
			element.removeEventListener('durationchange', metadata);
			element.removeEventListener('seeking', handleSeeking);
		};
	});
	let shareStatus = $state('');
	let shareFallbackUrl: string | null = $state(null);
	let shareInvalid = $state(false);
	let shareGeneration = 0;
	let shareAlive = true;
	let draggedLibraryItem: LibraryItem | null = null;
	// A link is consumed once per navigation, never replayed by Refresh or
	// across an account boundary. Deferred seeks belong to one loaded source.
	let handledShareSearch: string | null = null;
	let pendingShareSeek: {
		trackId: string;
		streamUrl: string;
		seconds: number;
		appliedSeconds: number | null;
	} | null = null;
	/**
	 * Favorites export feedback. Exporting is read-only: the handlers only
	 * snapshot `favoriteTracks` and write feedback here. The feedback is
	 * fenced to the arranged favorites it was produced from (see
	 * `favoritesExportCurrentKey` below): arranging, unstarring or an auth
	 * boundary retires it instead of leaving a stale success claim or a
	 * previous account's fallback text on screen.
	 */
	let favoritesExportStatus = $state('');
	let favoritesExportFallback: string | null = $state(null);
	let favoritesExportFeedbackKey: string | null = $state(null);
	let favoritesExportAlive = true;
	const favoritesExporter = createTrackListExport({
		getQueue: favoritesExportSnapshot,
		getKey: currentFavoritesExportKey,
		isActive: () => favoritesExportAlive,
		onOutcome: (text, manual) => {
			favoritesExportStatus = text;
			favoritesExportFallback = manual;
			favoritesExportFeedbackKey = text ? currentFavoritesExportKey() : null;
		},
		formatText: formatQueueText, formatM3U: formatQueueM3U,
		noun: 'favorites', filename: 'favorites.m3u', savedDownloads: true,
		unsafeMessage: 'Could not export favorites: a favorite has an unsafe track id.'
	});

	// Sources whose pending inbound-share seek was superseded by an
	// explicit restart (live position may still sit at the share target,
	// so a remembered position must be fenced here, not re-derived).
	const restartSupersededSources = new Set<string>();

	let trackMenu = $state<MenuRequest | null>(null);
	let refreshOwner: object | null = null;
	let libraryInputOwner: object | null = null;
	let libraryNotification = 0;

	const refresher = createLibraryRefresher({
		sessionStorage: () => browserStorage('session'),
		invalidateLibraryCache: () => {
			resetShareAtAuthBoundary();
			clearFavoritesExportFeedback();
			releaseFavoritesDownload();
			favoritesAnnouncement = '';
			resetRecentFinds();
			invalidateLibraryCache();
		},
		onStateChange: (state) => {
			// An aborted request still notifies in finally after route disposal.
			if (!shareAlive) return;
			const notification = ++libraryNotification;
			const owner = player.sourceInputOwner();
			const current = () => shareAlive && notification === libraryNotification && owner === player.sourceInputOwner();
			// Refreshed/auth-boundary snapshots invalidate captured intents.
			trackMenu = null;
			endTrackDrag();
			draggedLibraryItem = null;
			items = state.items;
			staleAt = state.staleAt;
			loading = state.loading;
			player.setQueueLiveRegions(!state.loading);
			if (!current()) return;
			loadError = state.loadError;
			libraryUnavailable = state.libraryUnavailable;
			requiresSignIn = state.requiresSignIn;
			// An access refusal that empties the library unmounts the queue
			// panel (alert branch); the route-owned filter text must go with
			// it so the next account never inherits the previous query.
			// Successful same-account refreshes keep items and keep the query.
			if (state.loadError && state.items.length === 0) {
				libraryInputOwner = null;
				player.setQueueFilter('');
				if (!current()) return;
				findQuery = '';
				findScope = 'both';
				view = 'all';
				groupByArtist = false;
				groupingAnnouncement = '';
				confirmClear = false;
				managementFeedback = '';
				recentFinds = [];
				recentFindsInitialized = false;
				recentFindsAnnouncement = '';
			}
			if (state.loading || state.items.length === 0) {
				clearShareFeedback();
				clearFavoritesExportFeedback();
				pendingShareSeek = null;
			}
			// Prune only against an authoritative successful catalogue or an
			// explicit access refusal. A remount's transient failure has no
			// verified snapshot and must not erase the layout-owned queue.
			// Auth transitions and 401/403 already reset the session above;
			// 404 explicitly clears through libraryUnavailable here.
			if (!state.loading && ((!state.loadError && state.staleAt === null) || state.libraryUnavailable)) {
				if (state.libraryUnavailable) libraryInputOwner = null;
				else if (refreshOwner === owner) libraryInputOwner = owner;
				else return; // The load began before this authenticated owner existed.
				const ids = new Set(state.items.map((track) => track.id));
				const kept = queue.tracks.filter((track) => ids.has(track.id));
				let next = queue;
				if (kept.length !== queue.tracks.length) {
					player.invalidateQueueConsent();
					if (!current()) return;
					const currentId = queueCurrentTrack(queue)?.id ?? null;
					const rebuilt = createQueue(kept);
					const index = currentId === null ? -1 : kept.findIndex((track) => track.id === currentId);
					next = index > 0 ? selectQueueIndex(rebuilt, index) : rebuilt;
				}
				if (next !== queue) retireQueueWork(next);
				if (!current()) return;
				// Pair authoritative evidence with membership, without hydrating rows
				// or touching a surviving lawful queued stream/decoder.
				if (!player.setQueue(next, { owner, tracks: sourceProjections(next), authoritative: true }) || !current()) return;
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
					if (selectedIds.length > 0 || appendFeedback !== null || rangeAnchorId !== null) {
						purgeSelection();
					}
				} else if (!state.loadError) {
					const snapshotIds = state.items.map((track) => track.id);
					selectedIds = pruneSelection(selectedIds, snapshotIds);
					if (rangeAnchorId !== null && !snapshotIds.includes(rangeAnchorId)) {
						rangeAnchorId = null;
					}
				}
			}
			// Bootstrap once after an authenticated load has minted the context.
			// Memory stays authoritative for this visit, even if persistence
			// fails; Refresh must not overwrite pending edits with stale storage.
			// Auth transitions and access refusals reset both memory and bootstrap.
			if (!state.loading && !state.loadError && !state.libraryUnavailable) {
				player.initializeFavorites();
				if (!current()) return;
				if (!recentFindsInitialized) {
					recentFinds = loadRecentFinds(browserStorage('session'), browserStorage('local'));
					recentFindsInitialized = true;
				}
				player.exposeQueueFeedback();
				if (!current()) return;
			}
			if (!state.loading) applyShareLink();
			if (!current()) return;
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
	// One-action bulk queue: the same final rendered ordered collection the
	// rows render, so sorting, filtering or grouping changes what an
	// activation appends. The count names only missing IDs, never the total.
	const bulkMissing = $derived(missingDisplayedTracks(renderedItems, queuedIds));
	const bulkDisabled = $derived(
		libraryUnavailable || renderedItems.length === 0 || bulkMissing.length === 0
	);
	const bulkAllQueued = $derived(
		!libraryUnavailable && renderedItems.length > 0 && bulkMissing.length === 0
	);
	// Play-from-top stays enabled while everything is queued: selecting the
	// first displayed track's slot is still useful.
	const playDisplayedDisabled = $derived(libraryUnavailable || renderedItems.length === 0);
	const streamUrl = $derived(resolveStreamUrl(currentTrack));
	const tabTitle = $derived(libraryTabTitle(currentTrack));
	// Captured before any title effect runs; the browser-only guard keeps SSR free of document access.
	const priorDocumentTitle = typeof document === 'undefined' ? null : document.title;
	const favoriteIds = $derived(new Set(favorites));
	// Favorites render in star order; ids absent from the current library
	// stay stored but unlisted (there is no title to show for them).
	const favoriteTracks = $derived(
		favorites
			.map((id) => items.find((track) => track.id === id) ?? null)
			.filter((track): track is LibraryItem => track !== null)
	);
	// Feedback stays visible only while the arranged favorites (ids and
	// display labels, in order) are exactly the ones it was exported from.
	const favoritesExportCurrentKey = $derived(currentFavoritesExportKey());
	const favoritesExportStatusShown = $derived(
		favoritesExportFeedbackKey !== null && favoritesExportFeedbackKey === favoritesExportCurrentKey
			? favoritesExportStatus
			: ''
	);
	const favoritesExportFallbackShown = $derived(
		favoritesExportFeedbackKey !== null && favoritesExportFeedbackKey === favoritesExportCurrentKey
			? favoritesExportFallback
			: null
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
		// Library display fields only: the bulk append and the favorites
		// export snapshot never carry a stream URL. Unknown extras are not a
		// verified loudness or served-rendition quality contract; keep them out.
		return {
			id: item.id,
			title: displayTitle(item),
			...(typeof item.title !== 'string' || !item.title.trim() ? { titleIsFallback: true } : {}),
			artist: displayArtist(item) ?? '',
			streamUrl: ''
		};
	}

	// The play path carries the item's stream URL like row selection does.
	// A separate one-parameter mapper (no positional flag), so a bare
	// `.map(...)` reference can never bind the array index to it.
	function playableTrackFor(item: LibraryItem): Track {
		return { ...queuedTrackFor(item), streamUrl: resolveStreamUrl({ streamUrl: item.streamUrl }) };
	}

	/**
	 * The append core shared by the bulk action and play-from-top: missing
	 * tracks join the end in displayed order, the existing arrangement and
	 * current selection survive, and an empty queue starts at its first row.
	 */
	function appendMissingTracks(missing: readonly Track[]): QueueState {
		return {
			tracks: [...queue.tracks, ...missing],
			currentIndex: queue.currentIndex < 0 ? 0 : queue.currentIndex
		};
	}

	function selectTrack(id: string, item = items.find((track) => track.id === id)): void {
		if (!libraryInputCurrent()) return;
		if (!item) return;
		clearShareFeedback();
		pendingShareSeek = null;
		// Preserve append-only row selection; only an inbound link explicitly
		// selects its target. Stream-less items still carry display fields only.
		replaceQueue(addTrack(queue, playableTrackFor(item)));
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
		if (missing.length === 0 || !libraryInputCurrent()) return;
		if (!replaceQueue(appendMissingTracks(missing.map(queuedTrackFor)))) return;
		player.setQueueFeedback(`Added ${missing.length} ${missing.length === 1 ? 'track' : 'tracks'} to the queue.`);
	}

	/**
	 * Non-destructive play-from-top: append the missing displayed tracks
	 * through the shared core, then select the first appended one (or the
	 * first displayed track's existing slot when everything is queued).
	 * Playback is attempted once, inside the gesture's activation window; a
	 * rejected play() leaves the track selected but paused and is reported
	 * as such — never retried. Any queue replacement after this activation
	 * (another action, an auth teardown) fences the late outcome.
	 */
	async function playDisplayedFromTop(): Promise<void> {
		if (playDisplayedDisabled || !libraryInputCurrent()) return;
		const missing = bulkMissing;
		const skipped = missingDisplayedTracks(renderedItems, new Set()).length - missing.length;
		// Appended IDs are new and unique, so one lookup finds the first
		// appended track, or the first displayed track's existing slot.
		const first = missing[0] ?? renderedItems[0];
		const base = appendMissingTracks(missing.map(playableTrackFor));
		const index = base.tracks.findIndex((track) => track.id === first?.id);
		const target = base.tracks[index];
		if (!target) return;
		// Bulk-added slots are metadata-only. This explicit play gesture
		// hydrates only its selected slot, preserving every other queued row.
		const source = resolveStreamUrl(target) || playableTrackFor(first).streamUrl;
		base.tracks[index] = { ...target, streamUrl: source };
		if (!replaceQueue(selectQueueIndex(base, index))) return;
		const counts = `Added ${missing.length}${skipped > 0 ? `, ${skipped} already queued` : ''}.`;
		await player.playQueuedTrack(target.id, counts);
	}

	function queueFavorites(): void {
		if (favoriteTracks.length === 0) return;
		const before = queue.tracks.length;
		// Use the shelf's resolved star order and the same deduping path as
		// individual taps. No persistence, playback, or account lookup here.
		for (const track of favoriteTracks) selectTrack(track.id, track);
		const added = queue.tracks.length - before;
		player.setQueueFeedback(`Queued ${added} ${added === 1 ? 'favorite' : 'favorites'}, ${favoriteTracks.length - added} already queued.`);
	}

	/** Snapshot the listed favorites, in arranged order, as queue display tracks. */
	function favoritesExportSnapshot(): QueueState {
		return createQueue(favoriteTracks.map(queuedTrackFor));
	}

	function currentFavoritesExportKey(): string {
		return favoritesExportSnapshot()
			.tracks.map((track) => `${track.id}\u0000${track.title}\u0000${track.artist}`)
			.join('\u0001');
	}

	function clearFavoritesExportFeedback(): void {
		favoritesExporter.reset();
	}

	const releaseFavoritesDownload = favoritesExporter.release;
	const copyFavoritesText = favoritesExporter.copy;
	const downloadFavoritesM3U = favoritesExporter.download;

	/**
	 * Swap the queue for any change that is not a move. The move
	 * announcement names a track and its position, so every other edit
	 * invalidates it; clearing it here also keeps a previous account's
	 * track title from lingering across the auth-boundary clears below.
	 */
	function retireQueueWork(next: QueueState, clearFeedback = false): void {
		// Clamped/reselected no-ops retire stale feedback, not pending route Play.
		if (next !== queue) playEpoch += 1;
		if (clearFeedback) player.setQueueFeedback('');
		const before = queueCurrentTrack(queue);
		const after = queueCurrentTrack(next);
		if (before?.id !== after?.id || before?.streamUrl !== after?.streamUrl) {
			clearShareFeedback();
			pendingShareSeek = null;
		}
	}

	function sourceProjections(next: QueueState, source: readonly LibraryItem[] = items): { id: string; streamUrl: string }[] {
		return next.tracks.flatMap((track) => {
			const item = source.find((item) => item.id === track.id);
			return item ? [{ id: item.id, streamUrl: playableTrackFor(item).streamUrl }] : [];
		});
	}

	function libraryInputCurrent(): boolean {
		return shareAlive && !loading && !libraryUnavailable && libraryInputOwner !== null &&
			libraryInputOwner === player.sourceInputOwner();
	}

	function replaceQueue(next: QueueState, source: readonly LibraryItem[] = items): boolean {
		if (!shareAlive) return false;
		const owner = libraryInputOwner;
		if (owner !== null && owner !== player.sourceInputOwner()) return false;
		retireQueueWork(next);
		if (!player.setQueue(next, owner === null ? undefined : { owner, tracks: sourceProjections(next, source) })) return false;
		player.setQueueFeedback('');
		if (!shareAlive || player.snapshot().queue !== next ||
			(owner !== null && owner !== player.sourceInputOwner())) return false;
		syncMiniPlayerHint(next);
		return true;
	}

	/**
	 * Mirror the queue's current selection into the mobile mini-player hint.
	 * Direct-assign queue edits bypass replaceQueue, so each of them calls
	 * this after mutating the queue; an emptied queue clears the hint.
	 * Metadata only — display title/artist, never audio or playback state.
	 */
	function syncMiniPlayerHint(next: QueueState): void {
		const current = queueCurrentTrack(next);
		if (current) {
			setMiniPlayerHint({
				trackId: current.id,
				title: displayTitle(current),
				artist: displayArtist(current)
			});
		} else {
			clearMiniPlayerHint();
		}
	}

	function clearShareFeedback(): void {
		shareGeneration += 1;
		shareStatus = '';
		shareFallbackUrl = null;
	}

	function resetShareAtAuthBoundary(): void {
		libraryInputOwner = null;
		// The session owns audio, but pending row actions and their feedback
		// remain page-owned and must be fenced before the previous play settles.
		playEpoch += 1;
		player.setQueueFeedback('');
		player.resetAtAuthBoundary();
		clearShareFeedback();
		pendingShareSeek = null;
		restartSupersededSources.clear();
		shareInvalid = false;
		if (typeof window !== 'undefined') handledShareSearch = window.location.search;
	}

	function shortTitle(title: string): string {
		// Count code points, not UTF-16 units: slicing between a surrogate pair
		// would announce a lone surrogate before the ellipsis.
		const points = Array.from(title);
		return points.length > 70 ? `${points.slice(0, 69).join('')}…` : title;
	}

	async function copyTrackLink(id: string, item = items.find((track) => track.id === id)): Promise<void> {
		clearShareFeedback();
		const generation = shareGeneration;
		if (!item || loading) return;
		const current = queueCurrentTrack(queue);
		const seconds =
			current?.id === id && resolveStreamUrl(current) ? audioEl?.currentTime : 0;
		if (typeof seconds !== 'number' || !Number.isFinite(seconds) || seconds < 0) return;
		let url: string;
		try {
			url = `${window.location.origin}/library?${buildShareQuery(id, seconds)}`;
		} catch {
			return;
		}
		const clipboard = clipboardWriter();
		if (!clipboard) {
			shareFallbackUrl = url;
			return;
		}
		try {
			await clipboard.writeText(url);
		} catch {
			if (shareAlive && generation === shareGeneration) shareFallbackUrl = url;
			return;
		}
		if (!shareAlive || generation !== shareGeneration) return;
		const title = shortTitle(displayTitle(item));
		shareStatus = `Copied link for ${title}.`;
	}

	function applyShareLink(): void {
		// A failed/unavailable load cannot resolve a link against the library.
		// Leave it unconsumed for Retry; auth resets still fence account changes.
		if (typeof window === 'undefined' || loading || loadError || libraryUnavailable) return;
		const search = window.location.search;
		if (search === handledShareSearch) return;
		handledShareSearch = search;
		pendingShareSeek = null;
		shareInvalid = false;
		const params = new URLSearchParams(search);
		if (!params.has('track') && !params.has('t')) return;
		const target = resolveShareTarget(
			{ track: params.get('track'), t: params.get('t') },
			items.map((track) => track.id)
		);
		if (
			!target ||
			!Number.isFinite(target.seconds) ||
			params.getAll('track').length !== 1 ||
			params.getAll('t').length !== 1
		) {
			shareInvalid = true;
			return;
		}
		// A newly consumed navigation must not inherit consent for the old arrangement.
		player.invalidateQueueConsent();
		selectTrack(target.trackId);
		replaceQueue(
			selectQueueIndex(queue, queue.tracks.findIndex((track) => track.id === target.trackId))
		);
		const source = resolveStreamUrl(queueCurrentTrack(queue));
		if (!source) return;
		// This navigation is a new gesture, not the seek that restart superseded.
		restartSupersededSources.delete(`${target.trackId}\n${source}`);
		pendingShareSeek = { ...target, streamUrl: source, appliedSeconds: null };
		// Same-source navigation also retires the persistent jump readout.
		player.retireJumpForShare(target.trackId, source);
		// Try after the keyed audio mounts; metadata/duration events retry if
		// the browser cannot seek yet and clamp a previously unknown duration.
		void tick().then(() => applyPendingShareSeek());
	}

	function applyPendingShareSeek(element = audioEl): void {
		const pending = pendingShareSeek;
		const current = queueCurrentTrack(queue);
		if (!pending || !element || element !== audioEl || !shareAlive) return;
		if (current?.id !== pending.trackId || resolveStreamUrl(current) !== pending.streamUrl) return;
		// An explicit restart of this source supersedes the remembered
		// share position: later metadata must not move it away from zero.
		if (restartSupersededSources.has(`${pending.trackId}\n${pending.streamUrl}`)) {
			pendingShareSeek = null;
			return;
		}
		const knownDuration = Number.isFinite(element.duration) && element.duration >= 0;
		const seconds = knownDuration ? Math.min(pending.seconds, element.duration) : pending.seconds;
		try {
			player.cancelPendingResume();
			pending.appliedSeconds = seconds;
			element.currentTime = seconds;
			if (knownDuration) pendingShareSeek = null;
		} catch {
			// Some browsers require metadata first. Keep this source-fenced seek
			// pending until its own metadata event, never play to force loading.
		}
	}

	function handleSeeking(event: Event): void {
		const element = audioEl;
		if (!element || event.currentTarget !== element) return;
		// A listener's own seek wins over a pending late-duration adjustment.
		if (
			pendingShareSeek?.appliedSeconds != null &&
			element.currentTime !== pendingShareSeek.appliedSeconds
		) {
			pendingShareSeek = null;
		}
	}

	// Both the visible control and guarded shortcut notify this route before
	// restarting, so later metadata cannot resurrect an inbound share seek.
	function supersedePositionSeek(trackId: string, source: string): void {
		if (pendingShareSeek?.trackId === trackId && pendingShareSeek.streamUrl === source) {
			pendingShareSeek = null;
		}
	}

	function supersedeShareSeek(trackId: string, source: string): void {
		supersedePositionSeek(trackId, source);
		restartSupersededSources.add(`${trackId}\n${source}`);
	}

	if (typeof window !== 'undefined') {
		afterNavigate(() => applyShareLink());
	}

	function toggleFavorite(id: string): void {
		if (!player.toggleFavorite(id)) return;
		favoritesAnnouncement = '';
		// Favorites-only unmounts an unstarred row, including its focused button.
		// All-tracks keeps that button mounted and must leave focus alone.
		if (view === 'favorites' && !favorites.includes(id)) focusLibraryView();
	}

	function focusLibraryView(): void {
		void tick().then(() => viewSelect?.focus({ preventScroll: true }));
	}

	function announceBrowse(text: string): void {
		browseAnnouncement = text;
		browseAnnouncementVersion += 1;
	}

	const VIEW_LABELS = {
		all: 'All tracks',
		favorites: 'Favorites only',
		selected: 'Selected only'
	} as const;

	function setView(next: typeof view): void {
		// Announce real scope changes only: reselecting the current view
		// changes nothing and stays silent.
		if (next === view) return;
		view = next;
		announceBrowse(`Showing ${VIEW_LABELS[next]}`);
	}

	function onOrderChange(event: Event): void {
		const select = event.currentTarget as HTMLSelectElement;
		const next = select.value as LibraryOrder;
		if (next === order) return;
		order = next;
		announceBrowse(`List ordered: ${select.selectedOptions[0]?.textContent?.trim() ?? next}`);
	}

	function onViewChange(event: Event): void {
		const next = (event.currentTarget as HTMLSelectElement).value;
		if (next === 'all' || next === 'favorites' || next === 'selected') setView(next);
	}

	function showAllTracks(): void {
		setView('all');
		focusLibraryView();
	}

	function afterQueuedFavorite(id: string): void {
		favoritesAnnouncement = '';
		if (view === 'favorites' && !favorites.includes(id)) focusLibraryView();
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
		event: Event | HTMLElement
	): Promise<void> {
		const button = event instanceof HTMLElement ? event : event.currentTarget;
		const visibleIds = favoriteTracks.map((track) => track.id);
		const next = moveFavoriteId(favorites, visibleIds, id, direction);
		if (next === favorites) return;
		if (!player.setFavorites([...next])) return;
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

	function searchBothFields(): void {
		findScope = 'both';
		// The recovery button only exists under a narrowed scope, so this
		// unmounts the focused button; land focus on the scope selector
		// (the control whose value just changed) instead of <body>.
		void tick().then(() => scopeSelect?.focus({ preventScroll: true }));
	}

	function resetRecentFinds(): void {
		recentFinds = [];
		recentFindsInitialized = false;
		recentFindsAnnouncement = '';
	}

	/**
	 * Announce a recent-find outcome within the 140-char polite cap. A
	 * max-length (120-char) query would breach it verbatim, so the embedded
	 * query is truncated with an ellipsis to fit; short queries announce whole.
	 */
	function announceRecentFind(verb: 'Saved' | 'Applied', query: string): string {
		const middle = ' recent search ';
		const budget = 140 - verb.length - middle.length - '.'.length;
		const shown = query.length > budget ? `${query.slice(0, budget - 1)}…` : query;
		return `${verb}${middle}${shown}.`;
	}

	/**
	 * Record the applied query when it is left. Memory is authoritative once
	 * bootstrapped; before that (or after an auth purge) there is no
	 * account-scoped list to extend, so nothing is recorded. Announces only
	 * a real change, never an empty, duplicate or already-first query.
	 */
	function recordFindQuery(): void {
		if (!recentFindsInitialized) return;
		const next = recordRecentFind(recentFinds, findQuery);
		if (next === recentFinds) return;
		recentFinds = [...next];
		saveRecentFinds(browserStorage('session'), browserStorage('local'), recentFinds);
		recentFindsAnnouncement = announceRecentFind('Saved', recentFinds[0]);
	}

	function applyRecentFind(query: string): void {
		// Preserve a pending unrecorded query first: a chip mousedown keeps
		// focus in the field (no blur), so record here before replacing it.
		recordFindQuery();
		findQuery = query;
		const next = recordRecentFind(recentFinds, query);
		if (next !== recentFinds) {
			recentFinds = [...next];
			saveRecentFinds(browserStorage('session'), browserStorage('local'), recentFinds);
		}
		recentFindsAnnouncement = announceRecentFind('Applied', query);
	}

	function clearRecentFinds(): void {
		recentFinds = [];
		saveRecentFinds(browserStorage('session'), browserStorage('local'), []);
		recentFindsAnnouncement = 'Cleared recent searches.';
		// The focused clear button unmounts with the list; keep focus in the section.
		void tick().then(() => recentFindsEmpty?.focus({ preventScroll: true }));
	}

	function clearFind(): void {
		// Announce real clears only: an already-empty find stays silent.
		if (findQuery !== '') announceBrowse('Search cleared');
		recordFindQuery();
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
		if (!player.setFavorites(favorites.filter((favorite) => favorite !== id))) return;
		favoritesAnnouncement = '';
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
		if (!player.setFavorites([])) return;
		favoritesAnnouncement = '';
		confirmClear = false;
		managementFeedback = 'Cleared local favorites.';
		void tick().then(() => clearButton?.focus({ preventScroll: true }));
	}

	/** ID of the library row holding focus, or null when focus is elsewhere. */
	function focusedLibraryRowId(): string | null {
		if (typeof document === 'undefined') return null;
		const holder = document.activeElement?.closest?.('[data-testid="library-item"]');
		return holder?.getAttribute('data-id') ?? null;
	}

	function focusLibraryRecovery(): void {
		const target = [viewSelect, refreshButton].find((element) =>
			element?.isConnected && !element.matches(':disabled'));
		target?.focus({ preventScroll: true });
	}

	/**
	 * After a selected row unmounts, land focus on the checkbox of the next
	 * surviving displayed row, else the previous one, else the Show
	 * control. Runs after rendering and only when the removed control took
	 * focus with it (active element fell back to the body), so controls
	 * that remain mounted — including the Clear selection button itself —
	 * and any focus the user has already moved keep it.
	 */
	function focusAfterSelectedRowRemoval(nextId: string | null, previousId: string | null): void {
		void tick().then(() => {
			if (typeof document === 'undefined') return;
			const active = document.activeElement;
			if (active && active !== document.body) return;
			for (const id of [nextId, previousId]) {
				if (id === null) continue;
				// Find the row by attribute comparison, never selector
				// interpolation: track ids are opaque server strings.
				const rowEl = [...document.querySelectorAll('[data-testid="library-item"]')].find(
					(element) => element.getAttribute('data-id') === id
				);
				const target = rowEl?.querySelector<HTMLElement>(
					'input[type="checkbox"]:not([disabled]), button:not([disabled])'
				);
				if (target) {
					target.focus({ preventScroll: true });
					return;
				}
			}
			focusLibraryRecovery();
		});
	}

	/**
	 * Checkbox activation: Shift extends from the anchor through the
	 * displayed rows (additive, so focus never leaves with a row); a plain
	 * activation, or Shift without a displayed anchor, toggles exactly one
	 * ID. Selection changes announce the count once; no-ops stay silent.
	 */
	function activateSelectionRow(id: string, extend: boolean, box: HTMLInputElement): void {
		selectInteractionRow(id, extend);
		// A native toggle the selection did not follow (Shift on a selected
		// anchor) would leave the box out of sync: re-assert the truth.
		box.checked = selectedIds.includes(id);
	}

	function selectInteractionRow(id: string, extend: boolean): void {
		const displayedIds = renderedItems.map((track) => track.id);
		if (extend && rangeAnchorId !== null && displayedIds.includes(rangeAnchorId)) {
			selectedIds = selectRange(selectedIds, displayedIds, rangeAnchorId, id);
		} else {
			// A missing displayed anchor toggles, and can remove the focused row.
			toggleSelectedId(id);
		}
		rangeAnchorId = id;
	}

	function toggleSelectedId(id: string): void {
		// Deselecting inside Selected only unmounts the row; when that row
		// held focus, recover it to a surviving row or the Show control.
		if (view !== 'selected' || !selectedIds.includes(id) || focusedLibraryRowId() !== id) {
			selectedIds = toggleSelected(selectedIds, id);
			return;
		}
		const displayedIds = renderedItems.map((track) => track.id);
		const removedIndex = displayedIds.indexOf(id);
		selectedIds = toggleSelected(selectedIds, id);
		const survivors = new Set(selectedIds);
		const nextId =
			removedIndex < 0
				? null
				: (displayedIds.slice(removedIndex + 1).find((candidate) => survivors.has(candidate)) ?? null);
		const previousId =
			removedIndex <= 0
				? null
				: ([...displayedIds.slice(0, removedIndex)]
						.reverse()
						.find((candidate) => survivors.has(candidate)) ?? null);
		focusAfterSelectedRowRemoval(nextId, previousId);
	}

	function selectDisplayedIds(): void {
		selectedIds = selectAllDisplayed(selectedIds, renderedItems.map((track) => track.id));
	}

	function clearSelectedIds(): void {
		// Selected only removes all rows. Recover only focus stranded by the
		// explicit clear; the mounted Clear selection button keeps its focus.
		const focusLeavesWithRows = view === 'selected' && focusedLibraryRowId() !== null;
		const focusLeavesWithToolbar = typeof document !== 'undefined' &&
			!!document.activeElement?.closest?.('[data-testid="track-selection-toolbar"]');
		const returnId = view === 'selected' ? null : renderedItems.find((track) => selectedIds.includes(track.id))?.id ?? null;
		// Announce real clears only: an already-empty selection stays silent.
		const hadSelection = selectedIds.length > 0;
		selectedIds = clearSelection(selectedIds);
		rangeAnchorId = null;
		if (hadSelection) announceBrowse('Selection cleared');
		if (focusLeavesWithRows || focusLeavesWithToolbar) focusAfterSelectedRowRemoval(returnId, null);
	}

	function appendSelectedIds(): void {
		if (!libraryInputCurrent()) return;
		const result = appendSelection(queue, selectedIds, items);
		if (!replaceQueue(result.queue)) return;
		appendFeedback = appendResultLabel(result.added, result.skipped);
	}


	function interactionIds(id: string): string[] {
		return selectedIds.includes(id) ? [...new Set(renderedItems.filter((item) => selectedIds.includes(item.id)).map((item) => item.id))] : [id];
	}

	// Re-resolve deferred actions after refresh without substituting another occurrence.
	function occurrenceItems(item: LibraryItem): LibraryItem[] {
		const key = libraryItemKey(item);
		const current = items.find((track) => libraryItemKey(track) === key);
		const others = items.filter((track) => track.id !== item.id);
		return current ? [current, ...others] : others;
	}

	function announceInteraction(text: string): void {
		player.setQueueFeedback(text, true);
	}

	function availableInteractionIds(ids: readonly string[], queued = false, source: readonly LibraryItem[] = items): string[] {
		const available = !libraryInputCurrent() ? [] : queued ? queue.tracks : source;
		const surviving = [...new Set(ids)].filter((id) => available.some((track) => track.id === id));
		if (!surviving.length) announceInteraction('That track is no longer available.');
		return surviving;
	}

	function missingInteractionLabel(ids: readonly string[], surviving: readonly string[]): string {
		const missing = new Set(ids).size - surviving.length;
		return missing ? ` ${missing} ${missing === 1 ? 'track is' : 'tracks are'} no longer available.` : '';
	}

	async function playLibraryIds(ids: readonly string[], catalogue: readonly LibraryItem[] = items): Promise<void> {
		const surviving = availableInteractionIds(ids, false, catalogue);
		const first = catalogue.find((item) => item.id === surviving[0]);
		if (!first) return;
		const added = surviving.filter((id) => !queue.tracks.some((track) => track.id === id)).length;
		// Play now moves the complete selection, including a previously current
		// member. Only Play next deliberately leaves the current track in place.
		const boundary = queue.tracks.slice(queue.currentIndex + 1).find((track) => !surviving.includes(track.id));
		const base = insertTracks(queue, knownTracks(surviving, catalogue), boundary?.id ?? null);
		const index = base.tracks.findIndex((track) => track.id === first.id);
		const target = base.tracks[index];
		if (!target) return;
		const source = resolveStreamUrl(target) || playableTrackFor(first).streamUrl;
		const tracks = [...base.tracks];
		tracks[index] = { ...target, streamUrl: source };
		if (!replaceQueue(selectQueueIndex({ ...base, tracks }, index), catalogue)) return;
		const counts = ids.length > 1 ? `Added ${added}${surviving.length > added ? `, ${surviving.length - added} already queued` : ''}.` : '';
		await player.playQueuedTrack(target.id, (counts + missingInteractionLabel(ids, surviving)).trim());
	}

	function appendInteractionIds(ids: readonly string[], source: readonly LibraryItem[] = items): void {
		const surviving = availableInteractionIds(ids, false, source);
		if (!surviving.length) return;
		const result = appendSelection(queue, surviving, source);
		if (!replaceQueue(result.queue, source)) return;
		announceInteraction(appendResultLabel(result.added, result.skipped) + missingInteractionLabel(ids, surviving));
	}

	function favoriteInteractionIds(ids: readonly string[], queued = false, source: readonly LibraryItem[] = items): void {
		const surviving = availableInteractionIds(ids, queued, source);
		if (!surviving.length) return;
		const remove = surviving.every((id) => favoriteIds.has(id));
		const changed = surviving.filter((id) => favoriteIds.has(id) === remove);
		const owner = libraryInputOwner;
		const next = remove ? favorites.filter((id) => !changed.includes(id)) : [...favorites, ...changed];
		if (!player.setFavorites(next) || !libraryInputCurrent() || owner !== libraryInputOwner || player.snapshot().favorites !== next) return;
		const already = surviving.length - changed.length;
		const count = `${changed.length} ${changed.length === 1 ? 'track' : 'tracks'}`;
		const track = (queued ? queue.tracks : source).find((track) => track.id === surviving[0]);
		const subject = ids.length === 1 && track ? displayTitle(track) : count;
		favoritesAnnouncement = (remove ? `Removed ${subject} from favorites.`
			: `Starred ${subject}${already ? `; ${already} ${already === 1 ? 'was' : 'were'} already starred` : ''}.`) + missingInteractionLabel(ids, surviving);
		const focusedId = focusedLibraryRowId();
		if (!queued && view === 'favorites' && remove && focusedId !== null && changed.includes(focusedId)) {
			focusAfterSelectedRowRemoval(null, null);
		}
	}

	function knownTracks(ids: readonly string[], source: readonly LibraryItem[] = items): Track[] {
		if (!libraryInputCurrent()) return [];
		return ids.flatMap((id) => {
			const item = source.find((item) => item.id === id);
			return item ? [queuedTrackFor(item)] : [];
		});
	}

	function queueNextIds(ids: readonly string[], source: readonly LibraryItem[] = items): void {
		const surviving = availableInteractionIds(ids, false, source);
		if (!surviving.length) return;
		const next = insertTracksNext(queue, knownTracks(surviving, source));
		if (next === queue) return;
		if (!replaceQueue(next, source)) return;
		announceInteraction('Tracks placed next in the queue.' + missingInteractionLabel(ids, surviving));
	}

	function insertQueuedIds(ids: readonly string[], beforeId: string | null): void {
		if (!libraryInputCurrent()) return;
		const source = draggedLibraryItem ? occurrenceItems(draggedLibraryItem) : items;
		draggedLibraryItem = null;
		const tracks = knownTracks(ids, source);
		// Copy from current loaded items, never drag payload URLs or menu captures.
		const inputs = tracks.flatMap((track) => {
			const item = source.find((item) => item.id === track.id);
			return item ? [{ id: item.id, streamUrl: playableTrackFor(item).streamUrl }] : [];
		});
		player.insertQueuedTracks(tracks, beforeId, { owner: libraryInputOwner!, tracks: inputs });
	}

	function playbackMenuActions(ids: readonly string[], source: () => readonly LibraryItem[] = () => items): MenuAction[] {
		return [
			{ label: 'Play now', run: () => void playLibraryIds(ids, source()) },
			{ label: 'Play next', run: () => queueNextIds(ids, source()) },
			{ label: 'Add to queue', run: () => appendInteractionIds(ids, source()) }
		];
	}

	function openTrackMenu(item: LibraryItem, invoker: HTMLElement, point?: { x: number; y: number }): void {
		if (loading || libraryUnavailable || !items.includes(item)) return;
		const id = item.id;
		const source = (): LibraryItem[] => occurrenceItems(item);
		const ids = interactionIds(id);
		const starred = ids.every((id) => favoriteIds.has(id));
		const actions = playbackMenuActions(ids, source);
		actions.push(
			{ label: `${starred ? 'Unstar' : 'Star'}${ids.length > 1 ? ' all' : ''}`, run: () => favoriteInteractionIds(ids, false, source()) },
			unavailablePlaylistAction
		);
		if (ids.length === 1) actions.push({ label: 'Copy link', run: () => {
			const current = source().find((track) => track.id === id);
			if (current && availableInteractionIds([id]).length) void copyTrackLink(id, current);
		} });
		const target = invoker.matches('li') ? invoker.querySelector<HTMLElement>('[data-testid="track-more"]') ?? invoker : invoker;
		const label = ids.length > 1 ? `${ids.length} selected tracks` : `Actions for ${displayTitle(item)}`;
		const displayedIds = renderedItems.map((item) => item.id);
		const index = displayedIds.indexOf(id);
		trackMenu = { ...menuAt(target, label, actions), fallbackFocus: () => {
			const surviving = new Set(renderedItems.map((item) => item.id));
			const next = displayedIds.slice(index + 1).find((id) => surviving.has(id)) ?? null;
			const previous = displayedIds.slice(0, index).reverse().find((id) => surviving.has(id)) ?? null;
			focusAfterSelectedRowRemoval(next, previous);
		}, ...point };
	}

	function openFavoriteMenu(id: string, invoker: HTMLElement, point?: { x: number; y: number }): void {
		const index = favoriteTracks.findIndex((track) => track.id === id);
		const track = favoriteTracks[index];
		if (loading || libraryUnavailable || !track) return;
		const target = invoker.matches('li') ? invoker.querySelector<HTMLElement>('[data-testid="favorite-more"]') ?? invoker : invoker;
		const actions = playbackMenuActions([id]);
		for (const direction of ['up', 'down'] as const) {
			if (direction === 'up' ? index === 0 : index === favoriteTracks.length - 1) continue;
			actions.push({ label: direction === 'up' ? 'Move up' : 'Move down', run: () => {
				if (!availableInteractionIds([id]).length || !favoriteIds.has(id)) return;
				void moveFavorite(id, direction, target);
			} });
		}
		actions.push(
			{ label: 'Unstar', run: () => {
				if (availableInteractionIds([id]).length && favoriteIds.has(id)) unstarFromShelf(id);
			} },
			unavailablePlaylistAction,
			{ label: 'Copy link', run: () => { if (availableInteractionIds([id]).length) void copyTrackLink(id); } }
		);
		trackMenu = { ...menuAt(target, `Actions for ${displayTitle(track)}`, actions), fallbackFocus: () => {
			if (shelfHeading?.isConnected) shelfHeading.focus({ preventScroll: true });
			else focusLibraryRecovery();
		}, ...point };
	}

	function libraryRowClick(event: MouseEvent, id: string): void {
		if (!event.ctrlKey && !event.metaKey && !event.shiftKey) return;
		if (event.target instanceof Element && event.target.closest('input, .nt-library-copy, .nt-library-star, .nt-library-more')) return;
		event.preventDefault();
		event.stopPropagation();
		selectInteractionRow(id, event.shiftKey);
	}

	function libraryListKey(event: KeyboardEvent): void {
		if (event.target instanceof Element && event.target.closest('input:not([type="checkbox"]), textarea, select, [contenteditable="true"]')) return;
		if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'a') {
			event.preventDefault();
			event.stopPropagation();
			selectDisplayedIds();
		} else if (event.key === ' ' && selectedIds.length > 0 && event.target instanceof Element && (event.target.matches('.nt-library-row') || !event.target.closest('input, button, a'))) {
			const id = event.target.closest('[data-testid="library-item"]')?.getAttribute('data-id');
			if (!id) return;
			event.preventDefault();
			event.stopPropagation();
			selectInteractionRow(id, event.shiftKey);
		}
	}

	function purgeSelection(): void {
		trackMenu = null;
		endTrackDrag();
		selectedIds = createSelection();
		rangeAnchorId = null;
		appendFeedback = null;
	}

	function refresh(): Promise<void> {
		if (!refresher.state.loading || refreshOwner === null) refreshOwner = player.sourceInputOwner();
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
		// Commit first: only a successful same-account refresh records the
		// preserved query (a failure or auth purge leaves history alone).
		if (!loadError && !libraryUnavailable) recordFindQuery();
		await tick();
		if (document.activeElement === null || document.activeElement === document.body) {
			refreshButton?.focus({ preventScroll: true });
		}
	}

	function clearLibraryAuthState(): void {
		const heldMenuFocus = typeof document !== 'undefined' &&
			!!document.activeElement?.closest('[data-testid="track-context-menu"]');
		replaceQueue(clearQueue(queue));
		clearSignedInEmail();
		clearRecentFindsAtAuthBoundary(browserStorage('session'), browserStorage('local'));
		favoritesAnnouncement = '';
		resetRecentFinds();
		player.setQueueFilter('');
		findQuery = '';
		findScope = 'both';
		view = 'all';
		groupByArtist = false;
		groupingAnnouncement = '';
		confirmClear = false;
		managementFeedback = '';
		purgeSelection();
		// The persistent queue's menu is outside this route, but its auth
		// invalidation still lands on Library's recovery control while mounted.
		if (heldMenuFocus) void tick().then(() => {
			if (document.activeElement === null || document.activeElement === document.body) {
				focusLibraryRecovery();
			}
		});
	}

	function handleAuthTransition(event: StorageEvent): void {
		// Session reset clears favorites BEFORE the refresher revokes its context.
		if (event.key === LIBRARY_AUTH_TRANSITION_KEY) {
			resetShareAtAuthBoundary();
			clearFavoritesExportFeedback();
			releaseFavoritesDownload();
			clearLibraryAuthState();
		}
		refresher.handleAuthTransition(event);
	}

	function handleLocalAuthTransition(): void {
		resetShareAtAuthBoundary();
		clearFavoritesExportFeedback();
		releaseFavoritesDownload();
		clearLibraryAuthState();
		refresher.handleLocalAuthTransition();
	}

	onMount(() => {
		// Restore only in the browser; SSR always renders Comfortable.
		density = readStoredDensity();
		player.setQueueDensity(density);
		window.addEventListener('storage', handleAuthTransition);
		// Leaving the page with a query applied (tab close, hard navigation).
		window.addEventListener('pagehide', recordFindQuery);
		const unsubscribeLocal = subscribeLocalAuthTransition(handleLocalAuthTransition);
		const unsubscribeRestart = player.onBeforeRestart(supersedeShareSeek);
		const unsubscribePosition = player.onBeforePositionIntent(supersedePositionSeek);
		const unsubscribeNavigation = player.onBeforeNavigate((next) => retireQueueWork(next, true));
		const unsubscribeFavorite = player.onQueuedFavorite(afterQueuedFavorite);
		const unsubscribeInsert = player.onQueueInsert(insertQueuedIds);
		// Discard the old shared localStorage snapshot from earlier versions.
		clearSnapshot(browserStorage('local'));
		void refresh();
		return () => {
			endTrackDrag();
			// Restore only while this page still owns the title; a newer route's title wins.
			if (priorDocumentTitle !== null && document.title === tabTitle) {
				document.title = priorDocumentTitle;
			}
			// Pending route Play must not publish into the persistent session after unmount.
			playEpoch += 1;
			shareAlive = false;
			libraryInputOwner = null;
			unsubscribeInsert();
			favoritesExportAlive = false;
			releaseFavoritesDownload();
			clearShareFeedback();
			pendingShareSeek = null;
			restartSupersededSources.clear();
			refresher.dispose();
			player.setQueueLiveRegions(true);
			// Client-side navigation away with the query applied.
			recordFindQuery();
			unsubscribeLocal();
			unsubscribeRestart();
			unsubscribePosition();
			unsubscribeNavigation();
			unsubscribeFavorite();
			window.removeEventListener('storage', handleAuthTransition);
			window.removeEventListener('pagehide', recordFindQuery);
		};
	});
</script>

<svelte:head>
	<title>{tabTitle}</title>
	<meta name="description" content="Your NNTune library, available even when the API is down." />
</svelte:head>

<main class="nt-wrap" data-density={density}>
	<h1>Library</h1>

	<div class="nt-library-density">
		<label for="library-density">Density</label>
		<select id="library-density" value={density} onchange={onDensityChange}>
			<option value="comfortable">Comfortable</option>
			<option value="compact">Compact</option>
		</select>
	</div>

	{#if shareInvalid}
		<p data-testid="library-share-invalid" role="alert">
			Invalid link — choose a track from your library.
		</p>
	{/if}
	<div data-testid="library-share-status" aria-live="polite" aria-atomic="true">{shareStatus}</div>
	<p data-testid="library-browse-status" class="nt-visually-hidden" aria-live="polite" aria-atomic="true">{#key browseAnnouncementVersion}{browseAnnouncement}{/key}</p>
	{#if shareFallbackUrl}
		<div class="nt-library-share-fallback">
			<label for="library-share-url">Track link</label>
			<input
				id="library-share-url"
				data-testid="library-share-fallback"
				aria-describedby="library-share-instructions"
				readonly
				value={shareFallbackUrl}
			/>
			<p id="library-share-instructions" data-testid="library-share-instructions">
				Select the link and press Ctrl+C or Cmd+C to copy it.
			</p>
		</div>
	{/if}

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
							{#each (['up', 'down'] as const) as direction (direction)}
								<button
									type="button"
									class="nt-favorite-move"
									aria-label={moveFavoriteLabel(track, direction)}
									disabled={index === (direction === 'up' ? 0 : favoriteTracks.length - 1)}
									onclick={(event) => void moveFavorite(track.id, direction, event)}
								>
									{direction === 'up' ? 'Up' : 'Down'}
								</button>
							{/each}
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
								><MoreActionsGlyph /></button>
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
			<button
				class="nt-button nt-bulk-queue-action"
				type="button"
				disabled={playDisplayedDisabled}
				aria-describedby={playDisplayedDisabled ? 'play-displayed-hint' : undefined}
				onclick={() => void playDisplayedFromTop()}
			>
				Play displayed from the top
			</button>
			{#if playDisplayedDisabled}
				<p id="play-displayed-hint">No displayed tracks to play.</p>
			{/if}
		</div>

		{#if libraryUnavailable}
			<p role="status">Library listing is not available yet.</p>
		{:else if items.length === 0}
			<p role="status">Your library is empty.</p>
		{:else}
			<p data-testid="library-track-count">{trackCountLabel(items.length)}</p>
			<div class="nt-library-find">
				<label for="library-find">Find in loaded tracks</label>
				<input
					id="library-find"
					type="search"
					bind:value={findQuery}
					aria-describedby="library-find-hint"
					onblur={recordFindQuery}
				/>
				<label for="library-find-scope">Search in</label>
				<select id="library-find-scope" bind:this={scopeSelect} bind:value={findScope}>
					<option value="both">Both title and artist</option>
					<option value="title">Title only</option>
					<option value="artist">Artist only</option>
				</select>
				<p id="library-find-hint">
					Searches only the tracks already loaded here, not the remote catalog.
				</p>
				{#if findQuery !== ''}
					<button class="nt-button" type="button" onclick={clearFind}>
						Clear search
					</button>
				{/if}
			</div>
			<!--
				Fixed-height rows (no wrapping chip row, hidden live text): recording
				on blur must not shift the list under a pointer that is mid-click.
			-->
			<section
				class="nt-library-recent-finds"
				data-testid="recent-finds"
				aria-labelledby="recent-finds-heading"
			>
				<div class="nt-recent-find-head">
					<h2 id="recent-finds-heading">Recent searches</h2>
					{#if recentFinds.length > 0}
						<button class="nt-button nt-button--ghost" type="button" onclick={clearRecentFinds}>
							Clear recent searches
						</button>
					{/if}
				</div>
				<p
					class="nt-visually-hidden"
					data-testid="recent-finds-announcement"
					aria-live="polite"
				>
					{recentFindsAnnouncement}
				</p>
				{#if recentFinds.length === 0}
					<p
						class="nt-recent-find-body"
						data-testid="recent-finds-empty"
						tabindex="-1"
						bind:this={recentFindsEmpty}
					>
						No recent searches yet.
					</p>
				{:else}
					<ul class="nt-recent-find-body nt-recent-find-list">
						{#each recentFinds as query (query)}
							<li>
								<!--
									Keep focus in the find field on mousedown: a blur here
									would record a pending query and shift every chip
									before mouseup, so the click would miss its chip.
									applyRecentFind records the pending query instead.
								-->
								<button
									class="nt-recent-find-chip"
									type="button"
									data-testid="recent-find-chip"
									onmousedown={(event) => event.preventDefault()}
									onclick={() => applyRecentFind(query)}
								>
									{query}
								</button>
							</li>
						{/each}
					</ul>
				{/if}
			</section>
			<div class="nt-library-order">
				<label for="library-order">Order tracks</label>
				<select id="library-order" value={order} onchange={onOrderChange} aria-describedby="library-order-hint">
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
				<select id="library-view" bind:this={viewSelect} value={view} onchange={onViewChange}>
					<option value="all">All tracks</option>
					<option value="favorites">Favorites only</option>
					<option value="selected">Selected only</option>
				</select>
			</div>
			<button
				class="nt-button"
				type="button"
				aria-pressed={groupByArtist}
				onclick={toggleArtistGrouping}
			>
				Group by artist
			</button>
			<p aria-live="polite" aria-atomic="true">{groupingAnnouncement}</p>
			<p data-testid="library-find-count" role={loading ? undefined : 'status'}>
				Showing {visibleItems.length} of {items.length} {visibleItems.length === 1 ? 'track' : 'tracks'}{findScope === 'title'
					? ' (title only)'
					: findScope === 'artist'
						? ' (artist only)'
						: ''}
			</p>
			{#if view === 'favorites' && displayedItems.length === 0}
				<p>
					No favorites in this list.
					<button type="button" onclick={showAllTracks}>Show all tracks</button>
				</p>
			{/if}
			{#if view === 'selected' && selectedIds.length === 0}
				<p>
					No tracks selected. Select tracks to review them here.
					<button type="button" onclick={showAllTracks}>Show all tracks</button>
				</p>
			{/if}
			<div class="nt-selection-bar" data-testid="track-selection-toolbar">
				<p data-testid="selection-count" role={loading ? undefined : 'status'}>{selectionCountLabel(selectedIds.length)}</p>
				{#if visibleItems.length > 0}
					<button class="nt-button" type="button" onclick={selectDisplayedIds}>
						Select all
					</button>
				{/if}
				<button class="nt-button" type="button" onclick={() => clearSelectedIds()}>
					Clear selection
				</button>
				<button class="nt-button" type="button" disabled={selectedIds.length === 0}
					onclick={() => queueNextIds(selectedIds)}>Play next</button>
				<button
					class="nt-button"
					type="button"
					disabled={selectedIds.length === 0}
					onclick={() => appendSelectedIds()}
				>
					Append selected
				</button>
				<SelectionExport {items} {selectedIds} />
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
				{#if findScope === 'title'}
					<p>No title matches in your loaded tracks.</p>
				{:else if findScope === 'artist'}
					<p>No artist matches in your loaded tracks.</p>
				{:else}
					<p>No matches in your loaded tracks.</p>
				{/if}
				{#if findScope !== 'both'}
					<button class="nt-button" type="button" onclick={searchBothFields}>
						Search both title and artist
					</button>
				{/if}
				<button class="nt-button" type="button" onclick={clearFind}>
					Clear search
				</button>
			{:else}
			<!-- svelte-ignore a11y_no_static_element_interactions: list-scoped shortcuts preserve all native controls and do not add a tab stop. -->
			<div data-testid="library-list" onkeydown={libraryListKey}>
				{#if groupByArtist}
					{#each artistGroups as group (group.key)}
						<div class="nt-artist-group">
							<h2>{group.label}</h2>
							{@render trackRows(group.items)}
						</div>
					{/each}
				{:else}
					{@render trackRows(visibleItems)}
				{/if}
			</div>
			{/if}
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

	{#if !loading && !loadError && !libraryUnavailable && items.length > 0}
		<div class="nt-library-repeat">
			<label for="library-repeat">Repeat</label>
			<select id="library-repeat" data-testid="player-repeat" value={repeat} onchange={onRepeatChange}>
				<option value="off">Off</option>
				<option value="one">One</option>
				<option value="queue">Queue</option>
			</select>
			<p data-testid="repeat-status" role="status" aria-atomic="true">Repeat: {repeat === 'off' ? 'Off' : repeat === 'one' ? 'One' : 'Queue'}</p>
		</div>
	{/if}

</main>

{#if trackMenu}
	{#key trackMenu}
		<ContextMenu request={trackMenu} onclose={() => (trackMenu = null)} />
	{/key}
{/if}

{#snippet trackRows(tracks: LibraryItem[])}
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
					draggedLibraryItem = track;
					startTrackDrag(event, interactionIds(track.id));
				}}
				ondragend={() => { draggedLibraryItem = null; }}
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
				><MoreActionsGlyph /></button>
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
{/snippet}


<style>
	.nt-library-item[data-selected] { background: var(--nt-bg-raised); outline: 1px solid var(--nt-accent); }
	.nt-track-list { padding: 0; list-style: none; }
	.nt-library-more { flex: 0 0 2.75rem; min-height: 40px; padding: 0.25rem; font: inherit; }
	@media (max-width: 480px) {
		.nt-track-list .nt-library-copy { width: 2.75rem; padding: 0.25rem; white-space: normal; }
	}
	.nt-favorite-more { font: inherit; padding: var(--nt-space-2); }
	[data-context-menu-longpress] { -webkit-touch-callout: none; user-select: none; }
	@media (pointer: coarse) {
		.nt-library-more, .nt-favorite-more, .nt-favorite-row, .nt-favorite-move, .nt-shelf-unstar { min-width: 48px; min-height: 48px; }
	}

	.nt-visually-hidden {
		position: absolute;
		width: 1px;
		height: 1px;
		margin: -1px;
		padding: 0;
		overflow: hidden;
		clip-path: inset(50%);
		white-space: nowrap;
		border: 0;
	}
	main {
		--nt-library-row-padding: 0.25rem;
	}
	main[data-density='compact'] {
		--nt-library-row-padding: 0;
	}
	.nt-library-repeat {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--nt-space-2) var(--nt-space-3);
		margin-block: var(--nt-space-3);
	}
	.nt-library-repeat select {
		max-width: 100%;
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}
	.nt-library-repeat select:focus-visible {
		outline: 2px solid var(--nt-accent);
		outline-offset: 2px;
	}
	.nt-library-density {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--nt-space-2) var(--nt-space-3);
	}
	.nt-library-density select {
		max-width: 100%;
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}
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
	.nt-library-find select {
		max-width: 100%;
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
	.nt-library-recent-finds {
		margin-block: var(--nt-space-2);
	}
	.nt-recent-find-head {
		display: flex;
		flex-wrap: nowrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--nt-space-2);
		min-height: 2.75rem;
	}
	.nt-recent-find-head h2 {
		font-size: 1rem;
		margin: 0;
	}
	.nt-recent-find-body {
		display: flex;
		align-items: center;
		min-height: 3.25rem;
		margin: 0;
	}
	.nt-recent-find-list {
		flex-wrap: nowrap;
		gap: var(--nt-space-2);
		list-style: none;
		padding: 4px;
		overflow-x: auto;
	}
	.nt-recent-find-list li {
		flex: 0 0 auto;
		max-width: 100%;
	}
	.nt-visually-hidden {
		position: absolute;
		width: 1px;
		height: 1px;
		margin: -1px;
		padding: 0;
		overflow: hidden;
		clip-path: inset(50%);
		white-space: nowrap;
	}
	.nt-recent-find-chip {
		min-width: 2.75rem;
		min-height: 2.75rem;
		max-width: 16rem;
		padding: var(--nt-space-1) var(--nt-space-3);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
		cursor: pointer;
	}
	.nt-recent-find-chip:focus-visible {
		outline: 2px solid var(--nt-accent);
		outline-offset: 2px;
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
	.nt-artist-group h2 {
		overflow-wrap: anywhere;
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
	.nt-library-share-fallback {
		margin-block: var(--nt-space-3);
	}
	.nt-library-share-fallback input {
		display: block;
		width: 100%;
		min-height: 2.75rem;
		padding: var(--nt-space-2) var(--nt-space-3);
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
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
