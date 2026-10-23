<script lang="ts">
	import { onMount } from 'svelte';
	import {
		browserStorage,
		clearSnapshot,
		invalidateLibraryCache,
		LIBRARY_AUTH_TRANSITION_KEY,
		staleLabel,
		type LibraryItem
	} from '#lib/library/cache.js';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import { createLibraryRefresher } from '#lib/library/refresh.js';
	import { resolveStreamUrl } from '#lib/library/stream-url.js';
	import { clearSignedInEmail } from '#lib/signed-in-email.js';
	import PlayerQueue from '#lib/player/PlayerQueue.svelte';
	import {
		addTrack,
		clearQueue,
		createQueue,
		currentTrack as queueCurrentTrack,
		moveTrack,
		nextTrack,
		playNext,
		removeTrack,
		selectTrack as selectQueueIndex,
		shuffleUpcoming,
		type QueueState
	} from '#lib/player/queue.js';

	let items: LibraryItem[] = $state([]);
	let staleAt: string | null = $state(null);
	let loading = $state(true);
	let loadError: string | null = $state(null);
	let libraryUnavailable = $state(false);
	let requiresSignIn = $state(false);
	let queue: QueueState = $state(createQueue([]));
	let queueAnnouncement = $state('');

	const refresher = createLibraryRefresher({
		sessionStorage: () => browserStorage('session'),
		invalidateLibraryCache,
		onStateChange: (state) => {
			items = state.items;
			staleAt = state.staleAt;
			loading = state.loading;
			loadError = state.loadError;
			libraryUnavailable = state.libraryUnavailable;
			requiresSignIn = state.requiresSignIn;
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

	function displayTitle(track: LibraryItem): string {
		const title = track.title;
		if (typeof title === 'string' && title.trim().length > 0) return title;
		return track.id;
	}

	function displayArtist(track: LibraryItem): string | null {
		const artist = track.artist;
		return typeof artist === 'string' && artist.trim().length > 0 ? artist : null;
	}

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

	function refresh(): Promise<void> {
		return refresher.refresh();
	}

	function handleAuthTransition(event: StorageEvent): void {
		// Clear the queue and this tab's identity label at the auth boundary
		// even before the refresher's state lands; the onStateChange
		// reconcile above is the backstop for the queue.
		if (event.key === LIBRARY_AUTH_TRANSITION_KEY) {
			replaceQueue(clearQueue(queue));
			clearSignedInEmail();
		}
		refresher.handleAuthTransition(event);
	}

	function handleLocalAuthTransition(): void {
		replaceQueue(clearQueue(queue));
		clearSignedInEmail();
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
		<button class="nt-button" type="button" onclick={() => void refresh()}>Retry</button>
	{:else}
		{#if staleAt}
			<p data-testid="library-stale-badge" role="status">{staleLabel(staleAt)}</p>
		{/if}

		{#if libraryUnavailable}
			<p role="status">Library listing is not available yet.</p>
			<p><a href="/library/import">Import an NZB</a></p>
		{:else if items.length === 0}
			<p role="status">Your library is empty. <a href="/library/import">Import an NZB</a> to get started.</p>
		{:else}
			<p data-testid="library-track-count">{trackCountLabel(items.length)}</p>
			<ul data-testid="library-list">
				{#each items as track (track.id)}
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
					</li>
				{/each}
			</ul>
			<PlayerQueue
				state={queue}
				announcement={queueAnnouncement}
				onnext={() => replaceQueue(nextTrack(queue))}
				onshuffle={() => replaceQueue(shuffleUpcoming(queue))}
				onmove={moveQueuedTrack}
				onplaynext={(index) => replaceQueue(playNext(queue, index))}
				onremove={(index) => replaceQueue(removeTrack(queue, index))}
				onclear={() => replaceQueue(clearQueue(queue))}
			/>
		{/if}

		<button class="nt-button nt-button--ghost" type="button" onclick={() => void refresh()}>
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
	.nt-library-item {
		padding: 0;
	}
	.nt-library-row {
		display: block;
		width: 100%;
		text-align: left;
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
