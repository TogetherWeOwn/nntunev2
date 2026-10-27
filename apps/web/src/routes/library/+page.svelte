<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { afterNavigate } from '$app/navigation';
	import { buildShareQuery, resolveShareTarget } from '#lib/player/shareTimestamp.js';
	import {
		browserStorage,
		clearSnapshot,
		invalidateLibraryCache,
		LIBRARY_AUTH_TRANSITION_KEY,
		LIBRARY_QUEUE_KEY,
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
	import { filterLibraryItems, type FindScope } from '#lib/library/filter.js';
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
		selectSelectedItems,
		selectionCountLabel,
		toggleSelected
	} from '#lib/library/selection.js';
	import { clearSignedInEmail } from '#lib/signed-in-email.js';
	import PlayerQueue from '#lib/player/PlayerQueue.svelte';
	import SelectionExport from '#lib/library/SelectionExport.svelte';
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
		restoreQueue,
		selectTrack as selectQueueIndex,
		serializeQueue,
		shuffleUpcoming,
		type QueueState,
		type Track
	} from '#lib/player/queue.js';

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
		try {
			window.localStorage.setItem(DENSITY_KEY, value);
		} catch {
			// Storage refused: keep the choice in memory without a banner.
		}
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
	let staleAt: string | null = $state(null);
	let loading = $state(true);
	let loadError: string | null = $state(null);
	let libraryUnavailable = $state(false);
	let requiresSignIn = $state(false);
	let queue: QueueState = $state(createQueue([]));
	// Bootstrap once from a fresh authenticated catalogue. Later refreshes
	// reconcile memory, never re-read a stale or failed storage write.
	let queueInitialized = false;
	let favorites: string[] = $state([]);
	let favoritesInitialized = false;
	let favoritesAnnouncement = $state('');
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
	let audioEl: HTMLAudioElement | undefined = $state();
	let shareStatus = $state('');
	let shareFallbackUrl: string | null = $state(null);
	let shareInvalid = $state(false);
	let shareGeneration = 0;
	let shareAlive = true;
	// A link is consumed once per navigation, never replayed by Refresh or
	// across an account boundary. Deferred seeks belong to one loaded source.
	let handledShareSearch: string | null = null;
	let pendingShareSeek: {
		trackId: string;
		streamUrl: string;
		seconds: number;
		appliedSeconds: number | null;
	} | null = null;
	// Restart availability is live native state of the mounted matching
	// element, re-evaluated on media events — never cached coordinates.
	// The outcome lives only in one polite status inside the Player region
	// and clears whenever the current source changes or an auth boundary
	// purges the queue, so a previous track or account never lingers.
	let restartCanSeek = $state(false);
	let restartStatus = $state('');
	let restartStatusVersion = $state(0);
	// Sources whose pending inbound-share seek was superseded by an
	// explicit restart (live position may still sit at the share target,
	// so a remembered position must be fenced here, not re-derived).
	const restartSupersededSources = new Set<string>();

	const refresher = createLibraryRefresher({
		sessionStorage: () => browserStorage('session'),
		invalidateLibraryCache: () => {
			resetShareAtAuthBoundary();
			replaceQueue(clearQueue(queue), false);
			purgeStoredQueue();
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
				findScope = 'both';
				view = 'all';
				confirmClear = false;
				managementFeedback = '';
			}
			if (state.loading || state.items.length === 0) {
				clearShareFeedback();
				pendingShareSeek = null;
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
			if (
				!queueInitialized && !state.loading && !state.loadError &&
				!state.libraryUnavailable && !state.requiresSignIn && state.staleAt === null
			) {
				// Validate before restoreQueue: its demo fallback is the full
				// catalogue, but an unrecognizable library queue must stay empty.
				try {
					const raw = browserStorage('session')?.getItem(LIBRARY_QUEUE_KEY) ?? null;
					// Bound JSON decoding before allocating an attacker-sized graph.
					if (raw !== null && raw.length <= 65_536) {
						const parsed = JSON.parse(raw) as { orderIds?: unknown; currentId?: unknown } | null;
						const ids = parsed?.orderIds;
						const known = new Set(state.items.map((track) => track.id));
						if (
							Array.isArray(ids) && ids.length <= state.items.length &&
							ids.every((id) => typeof id === 'string' && known.has(id)) &&
							new Set(ids).size === ids.length &&
							(parsed?.currentId == null || typeof parsed.currentId === 'string')
						) {
							const catalogue = state.items.map((item) => ({
								id: item.id,
								title: displayTitle(item),
								artist: displayArtist(item) ?? '',
								streamUrl: ''
							}));
							replaceQueue(restoreQueue(raw, catalogue), false);
						}
					}
				} catch {
					// Corrupt/blocked storage leaves the initially empty queue alone.
				}
				queueInitialized = true;
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
			if (!state.loading) applyShareLink();
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
		clearShareFeedback();
		pendingShareSeek = null;
		// Preserve append-only row selection; only an inbound link explicitly
		// selects its target. Stream-less items still carry display fields only.
		replaceQueue(
			addTrack(queue, {
				id: item.id,
				title: displayTitle(item),
				artist: displayArtist(item) ?? '',
				streamUrl: resolveStreamUrl({ streamUrl: item.streamUrl })
			})
		);
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
		// individual taps, including queue persistence. No playback or account lookup here.
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
	/**
	 * A current-source change invalidates the restart outcome and its
	 * availability snapshot, whichever queue edit caused it — including
	 * the direct-assign handlers that bypass replaceQueue, so a stale
	 * "Restarted …" can never describe the new current track.
	 */
	function resetRestartOnSourceChange(
		before: Track | null | undefined,
		after: Track | null | undefined
	): void {
		if (before?.id !== after?.id || before?.streamUrl !== after?.streamUrl) {
			restartCanSeek = false;
			restartStatus = '';
		}
	}

	function replaceQueue(next: QueueState, persist = true): void {
		const before = queueCurrentTrack(queue);
		const after = queueCurrentTrack(next);
		if (before?.id !== after?.id || before?.streamUrl !== after?.streamUrl) {
			clearShareFeedback();
			pendingShareSeek = null;
		}
		resetRestartOnSourceChange(before, after);
		queue = next;
		queueAnnouncement = '';
		if (persist) persistQueue();
	}

	function persistQueue(): void {
		if (!queueInitialized) return;
		try {
			const storage = browserStorage('session');
			if (!storage) return;
			const raw = serializeQueue(queue);
			if (raw.length <= 65_536) storage.setItem(LIBRARY_QUEUE_KEY, raw);
			else storage.removeItem(LIBRARY_QUEUE_KEY);
		} catch {
			// Memory remains authoritative for this visit if storage fails.
		}
	}

	function purgeStoredQueue(): void {
		// Do not bootstrap again in this visit, even if removal is blocked.
		queueInitialized = true;
		try {
			browserStorage('session')?.removeItem(LIBRARY_QUEUE_KEY);
		} catch {
			// Best effort in privacy-restricted browser contexts.
		}
	}

	function clearShareFeedback(): void {
		shareGeneration += 1;
		shareStatus = '';
		shareFallbackUrl = null;
	}

	function resetShareAtAuthBoundary(): void {
		clearShareFeedback();
		pendingShareSeek = null;
		restartSupersededSources.clear();
		restartCanSeek = false;
		restartStatus = '';
		shareInvalid = false;
		if (typeof window !== 'undefined') handledShareSearch = window.location.search;
	}

	async function copyTrackLink(id: string): Promise<void> {
		clearShareFeedback();
		const generation = shareGeneration;
		const item = items.find((track) => track.id === id);
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
		// Match demo copy: feature-detect inside a try (even the getter may
		// throw), and offer manual copy for missing/denied clipboard access.
		type ClipboardWriter = { writeText: (text: string) => Promise<void> };
		let clipboard: ClipboardWriter | undefined;
		try {
			const candidate = navigator.clipboard;
			clipboard = typeof candidate?.writeText === 'function' ? candidate : undefined;
		} catch {
			clipboard = undefined;
		}
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
		const title = displayTitle(item);
		shareStatus = `Copied link for ${title.length > 70 ? `${title.slice(0, 69)}…` : title}.`;
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
		selectTrack(target.trackId);
		replaceQueue(
			selectQueueIndex(queue, queue.tracks.findIndex((track) => track.id === target.trackId))
		);
		const source = resolveStreamUrl(queueCurrentTrack(queue));
		if (!source) return;
		// This navigation is a new gesture, not the seek that restart superseded.
		restartSupersededSources.delete(`${target.trackId}\n${source}`);
		pendingShareSeek = { ...target, streamUrl: source, appliedSeconds: null };
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
			pending.appliedSeconds = seconds;
			element.currentTime = seconds;
			if (knownDuration) pendingShareSeek = null;
		} catch {
			// Some browsers require metadata first. Keep this source-fenced seek
			// pending until its own metadata event, never play to force loading.
		}
	}

	function handleSeeking(): void {
		// A listener's own seek wins over a pending late-duration adjustment.
		if (
			pendingShareSeek?.appliedSeconds != null &&
			audioEl?.currentTime !== pendingShareSeek.appliedSeconds
		) {
			pendingShareSeek = null;
		}
	}

	// True when position 0 lies inside a native seekable range. Any unreadable
	// or non-finite range bound is unusable media, never an exception.
	function seekableIncludesStart(element: HTMLAudioElement): boolean {
		try {
			const ranges = element.seekable;
			for (let index = 0; index < ranges.length; index += 1) {
				const start = ranges.start(index);
				const end = ranges.end(index);
				if (Number.isFinite(start) && Number.isFinite(end) && start <= 0 && end >= 0) {
					return true;
				}
			}
		} catch {
			// Fall through: an unreadable range set cannot restart.
		}
		return false;
	}

	// Media events re-evaluate restart availability from the live element.
	// Events from a detached or replaced element are ignored, so stale
	// nodes can never enable, disable, or complete a restart.
	function refreshRestartAvailability(event: Event): void {
		const element = event.currentTarget as HTMLAudioElement | null;
		if (!element || element !== audioEl || !shareAlive) return;
		const current = queueCurrentTrack(queue);
		if (!current || resolveStreamUrl(current) !== element.getAttribute('src')) {
			restartCanSeek = false;
			return;
		}
		restartCanSeek = restartIsAvailable(element);
	}

	function restartIsAvailable(element: HTMLAudioElement): boolean {
		try {
			return (
				Number.isFinite(element.duration) &&
				element.duration > 0 &&
				Number.isFinite(element.currentTime) &&
				seekableIncludesStart(element)
			);
		} catch {
			return false;
		}
	}

	function setRestartStatus(text: string): void {
		restartStatus = text;
		restartStatusVersion += 1;
	}

	/**
	 * Explicit restart: seek the currently mounted matching element to zero
	 * and claim success only on readback. Touches nothing but currentTime,
	 * so paused/playing intent, source, queue/current identity, rate and
	 * volume survive; never calls play/pause/load, never advances tracks,
	 * and needs no keyboard shortcut. A throwing or silently-ignoring
	 * setter is a truthful failure, not success inferred from the write.
	 * The gesture supersedes any pending inbound-share seek for the source.
	 */
	function restartCurrentTrack(): void {
		const element = audioEl;
		const current = queueCurrentTrack(queue);
		if (!element || !current) return;
		const source = resolveStreamUrl(current);
		if (!source || element.getAttribute('src') !== source) return;
		const title = current.title?.trim() ? current.title : current.id;
		const withTitle = (words: string): string => {
			const suffix = ` ${title}.`;
			if (words.length + suffix.length <= 99) return `${words}${suffix}`;
			let end = 97 - words.length;
			// Never cut between the halves of a surrogate pair.
			const last = title.charCodeAt(end - 1);
			if (last >= 0xd800 && last <= 0xdbff) end -= 1;
			return `${words} ${title.slice(0, end)}…`;
		};
		// The explicit gesture supersedes a pending share seek for this
		// source even if the native write is refused: later metadata for
		// the same source must never move the position to the share time.
		if (pendingShareSeek?.trackId === current.id && pendingShareSeek?.streamUrl === source) {
			pendingShareSeek = null;
		}
		restartSupersededSources.add(`${current.id}\n${source}`);
		// Native state can change after the last media event and before this click.
		if (!restartIsAvailable(element)) {
			restartCanSeek = false;
			setRestartStatus(withTitle('Could not restart'));
			return;
		}
		let position = Number.NaN;
		try {
			position = element.currentTime;
		} catch {
			// Handled below: an unreadable position cannot restart.
		}
		if (!Number.isFinite(position)) {
			setRestartStatus(withTitle('Could not restart'));
			return;
		}
		if (Math.abs(position) <= 0.05) {
			try {
				element.currentTime = 0;
			} catch {
				// Already at the start; a refusal changes nothing there.
			}
			setRestartStatus(withTitle('Already at the start of'));
			return;
		}
		let refused = false;
		try {
			element.currentTime = 0;
		} catch {
			refused = true;
		}
		let readback = Number.NaN;
		if (!refused) {
			try {
				readback = element.currentTime;
			} catch {
				// Handled below: no readback means no success claim.
			}
		}
		setRestartStatus(
			!refused && Number.isFinite(readback) && Math.abs(readback) <= 0.05
				? withTitle('Restarted')
				: withTitle('Could not restart')
		);
	}

	if (typeof window !== 'undefined') {
		afterNavigate(() => applyShareLink());
	}

	function moveQueuedTrack(from: number, to: number): void {
		const moved = queue.tracks[from];
		if (moved === undefined) return;
		const reordered = moveTrack(queue, from, to);
		// Announce real moves only: a no-op reorder must not chatter the
		// live region.
		if (reordered === queue) return;
		queue = reordered;
		persistQueue();
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
		// replaceQueue resets the restart outcome for the removed track when
		// the current source advances, and persists the removal.
		replaceQueue(next);
		queueAnnouncement = `${title} removed from the queue.`;
		queueAnnouncementVersion += 1;
	}

	function clearQueuedTracks(): void {
		const next = clearQueue(queue);
		// Announce real clears only: a no-op clear must not chatter the live
		// region.
		if (next === queue) return;
		replaceQueue(next);
		queueAnnouncement = 'Queue cleared.';
		queueAnnouncementVersion += 1;
	}

	/**
	 * Metadata-only direct selection: apply the validated selectTrack
	 * transition on the ORIGINAL queue coordinate, preserving every track id
	 * and arrangement, persisting only the queue's IDs. Never transport,
	 * audio, provider or stream language. Reselecting the current row keeps
	 * the existing feedback; stale or invalid coordinates select nothing.
	 */
	function selectQueuedTrack(index: number): void {
		if (!Number.isInteger(index) || index < 0 || index >= queue.tracks.length) return;
		if (index === queue.currentIndex) return;
		const target = queue.tracks[index];
		if (target === undefined) return;
		const selected = selectQueueIndex(queue, index);
		if (selected === queue) return;
		// Direct selection always changes the current source (reselecting
		// the current row returned above); replaceQueue resets restart
		// state and persists the selection.
		replaceQueue(selected);
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
		persistQueue();
		queueAnnouncement = `Removed ${removed} ${removed === 1 ? 'track' : 'tracks'} before the current track.`;
	}

	function trimUpcomingTracks(): void {
		const trimmed = clearUpcoming(queue);
		// Announce real trims only: a no-op trim must not chatter the live
		// region.
		if (trimmed === queue) return;
		const removed = queue.tracks.length - trimmed.tracks.length;
		queue = trimmed;
		persistQueue();
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

	function searchBothFields(): void {
		findScope = 'both';
		// The recovery button only exists under a narrowed scope, so this
		// unmounts the focused button; land focus on the scope selector
		// (the control whose value just changed) instead of <body>.
		void tick().then(() => scopeSelect?.focus({ preventScroll: true }));
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

	/** ID of the library row holding focus, or null when focus is elsewhere. */
	function focusedLibraryRowId(): string | null {
		if (typeof document === 'undefined') return null;
		const holder = document.activeElement?.closest?.('[data-testid="library-item"]');
		return holder?.getAttribute('data-id') ?? null;
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
			viewSelect?.focus({ preventScroll: true });
		});
	}

	function toggleSelectedId(id: string): void {
		// Deselecting inside Selected only unmounts the row; when that row
		// held focus, recover it to a surviving row or the Show control.
		if (view !== 'selected' || !selectedIds.includes(id) || focusedLibraryRowId() !== id) {
			selectedIds = toggleSelected(selectedIds, id);
			return;
		}
		const displayedIds = visibleItems.map((track) => track.id);
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
		selectedIds = selectAllDisplayed(selectedIds, visibleItems.map((track) => track.id));
	}

	function clearSelectedIds(): void {
		// Clearing inside Selected only unmounts every row; only focus that
		// sat inside one of them needs the Show-control fallback.
		const focusLeavesWithRows = view === 'selected' && focusedLibraryRowId() !== null;
		selectedIds = clearSelection(selectedIds);
		if (focusLeavesWithRows) focusAfterSelectedRowRemoval(null, null);
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
			resetShareAtAuthBoundary();
			replaceQueue(clearQueue(queue), false);
			purgeStoredQueue();
			clearSignedInEmail();
			// Clear favorites storage BEFORE the refresher revokes the
			// session auth context the scoped key derives from.
			clearFavoritesAtAuthBoundary(browserStorage('session'), browserStorage('local'));
			favorites = [];
			favoritesInitialized = false;
			favoritesAnnouncement = '';
			queueFilterQuery = '';
			findQuery = '';
			findScope = 'both';
			view = 'all';
			confirmClear = false;
			managementFeedback = '';
			purgeSelection();
		}
		refresher.handleAuthTransition(event);
	}

	function handleLocalAuthTransition(): void {
		resetShareAtAuthBoundary();
		replaceQueue(clearQueue(queue), false);
		purgeStoredQueue();
		clearSignedInEmail();
		clearFavoritesAtAuthBoundary(browserStorage('session'), browserStorage('local'));
		favorites = [];
		favoritesInitialized = false;
		favoritesAnnouncement = '';
		queueFilterQuery = '';
		findQuery = '';
		findScope = 'both';
		view = 'all';
		confirmClear = false;
		managementFeedback = '';
		purgeSelection();
		refresher.handleLocalAuthTransition();
	}

	onMount(() => {
		// Restore only in the browser; SSR always renders Comfortable.
		density = readStoredDensity();
		window.addEventListener('storage', handleAuthTransition);
		const unsubscribeLocal = subscribeLocalAuthTransition(handleLocalAuthTransition);
		// Discard the old shared localStorage snapshot from earlier versions.
		clearSnapshot(browserStorage('local'));
		void refresh();
		return () => {
			// Restore only while this page still owns the title; a newer route's title wins.
			if (priorDocumentTitle !== null && document.title === tabTitle) {
				document.title = priorDocumentTitle;
			}
			shareAlive = false;
			clearShareFeedback();
			pendingShareSeek = null;
			restartSupersededSources.clear();
			refresher.dispose();
			unsubscribeLocal();
			window.removeEventListener('storage', handleAuthTransition);
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
					<option value="selected">Selected only</option>
				</select>
			</div>
			<p data-testid="library-find-count">
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
			<div class="nt-selection-bar">
				<p data-testid="selection-count" role={loading ? undefined : 'status'}>{selectionCountLabel(selectedIds.length)}</p>
				{#if visibleItems.length > 0}
					<button class="nt-button" type="button" onclick={selectDisplayedIds}>
						Select all
					</button>
				{/if}
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
							class="nt-library-copy"
							data-testid="library-copy-link"
							aria-label={`Copy link ${displayTitle(track)}`}
							onclick={() => void copyTrackLink(track.id)}
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
			<div class="nt-library-player-controls">
				{#key `${currentTrack?.id}:${streamUrl}`}
					<audio
						bind:this={audioEl}
						data-testid="audio-player"
						controls
						src={streamUrl}
						onloadedmetadata={(event) => {
							applyPendingShareSeek(event.currentTarget);
							refreshRestartAvailability(event);
						}}
						ondurationchange={(event) => {
							applyPendingShareSeek(event.currentTarget);
							refreshRestartAvailability(event);
						}}
						oncanplay={refreshRestartAvailability}
						onprogress={refreshRestartAvailability}
						ontimeupdate={refreshRestartAvailability}
						onseeking={handleSeeking}
					></audio>
				{/key}
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
			{#if !restartCanSeek}
				<p id="library-restart-hint">Restart is unavailable until this track can seek.</p>
			{/if}
			<p data-testid="library-restart-status" aria-live="polite">{#key restartStatusVersion}{restartStatus}{/key}</p>
		</section>
	{/if}
</main>

<style>
	main {
		--nt-library-row-padding: 0.25rem;
	}
	main[data-density='compact'] {
		--nt-library-row-padding: 0;
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
	/* Density only changes row whitespace, never the controls or their order.
	 * The ancestor scope keeps the shared queue's other hosts unchanged. */
	main :global(.queue-row) {
		padding-block: var(--nt-library-row-padding);
	}
	main :global(.queue-row button:not(.queue-star)) {
		min-width: 24px;
		min-height: 24px;
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
	.nt-player-identity {
		overflow-wrap: anywhere;
	}
	audio {
		max-width: 100%;
	}
	.nt-library-player-controls {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--nt-space-2) var(--nt-space-3);
	}
	.nt-library-player-controls audio {
		flex: 1 1 12rem;
		min-width: 0;
	}
	.nt-library-restart {
		flex: 0 0 auto;
		min-height: 2.75rem;
		border: 0;
		cursor: pointer;
		font: inherit;
	}
	.nt-library-restart:disabled {
		cursor: default;
		opacity: 0.5;
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
