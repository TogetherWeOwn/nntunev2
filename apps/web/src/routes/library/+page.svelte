<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { afterNavigate } from '$app/navigation';
	import { buildShareQuery, resolveShareTarget } from '#lib/player/shareTimestamp.js';
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
	import { createLibraryRefresher } from '#lib/library/refresh.js';
	import {
		displayArtist,
		displayTitle,
		orderLibraryItems,
		type LibraryOrder
	} from '#lib/library/order.js';
	import { resolveStreamUrl } from '#lib/library/stream-url.js';
	import {
		appendResultLabel,
		appendSelection,
		clearSelection,
		createSelection,
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
	/**
	 * Explicit subset selection: page-local ordered stable IDs in checkbox
	 * activation order. Never persisted, never fetched, never shared across
	 * tabs; purged only at auth/account boundaries, pruned only against a
	 * successful snapshot. The last append outcome stays until the next
	 * append or a boundary purge.
	 */
	let selectedIds: string[] = $state(createSelection());
	let appendFeedback: string | null = $state(null);
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

	const refresher = createLibraryRefresher({
		sessionStorage: () => browserStorage('session'),
		invalidateLibraryCache: () => {
			resetShareAtAuthBoundary();
			favorites = [];
			favoritesInitialized = false;
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
		const before = queueCurrentTrack(queue);
		const after = queueCurrentTrack(next);
		if (before?.id !== after?.id || before?.streamUrl !== after?.streamUrl) {
			clearShareFeedback();
			pendingShareSeek = null;
		}
		queue = next;
		queueAnnouncement = '';
	}

	function clearShareFeedback(): void {
		shareGeneration += 1;
		shareStatus = '';
		shareFallbackUrl = null;
	}

	function resetShareAtAuthBoundary(): void {
		clearShareFeedback();
		pendingShareSeek = null;
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
		if (typeof window === 'undefined' || loading) return;
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
			resetShareAtAuthBoundary();
			replaceQueue(clearQueue(queue));
			clearSignedInEmail();
			// Clear favorites storage BEFORE the refresher revokes the
			// session auth context the scoped key derives from.
			clearFavoritesAtAuthBoundary(browserStorage('session'), browserStorage('local'));
			favorites = [];
			favoritesInitialized = false;
			queueFilterQuery = '';
			purgeSelection();
		}
		refresher.handleAuthTransition(event);
	}

	function handleLocalAuthTransition(): void {
		resetShareAtAuthBoundary();
		replaceQueue(clearQueue(queue));
		clearSignedInEmail();
		clearFavoritesAtAuthBoundary(browserStorage('session'), browserStorage('local'));
		favorites = [];
		favoritesInitialized = false;
		queueFilterQuery = '';
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
			shareAlive = false;
			clearShareFeedback();
			pendingShareSeek = null;
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
			{#if favoriteTracks.length === 0}
				<p data-testid="favorites-empty">No favorites yet. Star a track to pin it here.</p>
			{:else}
				<button class="nt-button" type="button" onclick={queueFavorites}>Queue favorites</button>
				<ul>
					{#each favoriteTracks as track (track.id)}
						<li data-testid="favorite-item" data-id={track.id}>
							<button type="button" onclick={() => selectTrack(track.id)}>
								{displayTitle(track)}
								{#if displayArtist(track)}
									<span> — {displayArtist(track)}</span>
								{/if}
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
			</div>
			<ul data-testid="library-list">
				{#each orderedItems as track (track.id)}
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
			{#if currentTrack}
				<p class="nt-player-identity" data-testid="library-now-playing">
					{displayTitle(currentTrack)}{#if displayArtist(currentTrack)}{' — '}{displayArtist(currentTrack)}{/if}
				</p>
			{/if}
			{#key `${currentTrack?.id}:${streamUrl}`}
				<audio
					bind:this={audioEl}
					data-testid="audio-player"
					controls
					src={streamUrl}
					onloadedmetadata={(event) => applyPendingShareSeek(event.currentTarget)}
					ondurationchange={(event) => applyPendingShareSeek(event.currentTarget)}
					onseeking={handleSeeking}
				></audio>
			{/key}
		</section>
	{/if}
</main>

<style>
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
