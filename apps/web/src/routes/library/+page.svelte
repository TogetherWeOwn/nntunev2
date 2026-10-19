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
	import { createLibraryRefresher } from '#lib/library/refresh.js';

	let items: LibraryItem[] = $state([]);
	let staleAt: string | null = $state(null);
	let loading = $state(true);
	let loadError: string | null = $state(null);
	let libraryUnavailable = $state(false);
	let selectedIds: string[] = $state([]);

	const refresher = createLibraryRefresher({
		sessionStorage: () => browserStorage('session'),
		invalidateLibraryCache,
		onStateChange: (state) => {
			items = state.items;
			staleAt = state.staleAt;
			loading = state.loading;
			loadError = state.loadError;
			libraryUnavailable = state.libraryUnavailable;
			// Selections are per-account: drop ids the new library no longer
			// contains, so a 401/403/404 or auth transition cannot leak the
			// previous account's queue into the next session's Retry.
			if (selectedIds.length > 0) {
				const ids = new Set(state.items.map((track) => track.id));
				selectedIds = selectedIds.filter((id) => ids.has(id));
			}
		}
	});

	// e2e contract hooks (e2e/helpers/contract.ts TESTIDS) owned by sibling
	// slices: usage metering, plan-limit enforcement and the player land with
	// the venture slices that own them. Placeholders below keep the hooks
	// rendered until then.
	const overLimit = false;
	const currentTrack: { id: string; title: string } | null = null;

	function displayTitle(track: LibraryItem): string {
		const title = track.title;
		if (typeof title === 'string' && title.trim().length > 0) return title;
		return track.id;
	}

	function displayArtist(track: LibraryItem): string | null {
		const artist = track.artist;
		return typeof artist === 'string' && artist.trim().length > 0 ? artist : null;
	}

	function selectTrack(id: string): void {
		if (!selectedIds.includes(id)) selectedIds = [...selectedIds, id];
	}

	function selectedTrack(id: string): LibraryItem {
		return items.find((track) => track.id === id) ?? { id };
	}

	function refresh(): Promise<void> {
		return refresher.refresh();
	}

	function handleAuthTransition(event: StorageEvent): void {
		// Clear selections at the auth boundary even before the refresher's
		// state lands; the onStateChange reconcile above is the backstop.
		if (event.key === LIBRARY_AUTH_TRANSITION_KEY) selectedIds = [];
		refresher.handleAuthTransition(event);
	}

	onMount(() => {
		window.addEventListener('storage', handleAuthTransition);
		// Discard the old shared localStorage snapshot from earlier versions.
		clearSnapshot(browserStorage('local'));
		void refresh();
		return () => {
			refresher.dispose();
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
			<ul data-testid="library-list">
				{#each items as track (track.id)}
					<li class="nt-library-item" data-testid="library-item" data-id={track.id}>
						<button
							type="button"
							class="nt-library-row"
							onclick={() => selectTrack(track.id)}
						>
							{displayTitle(track)}
							{#if displayArtist(track)}
								<span> — {displayArtist(track)}</span>
							{/if}
						</button>
					</li>
				{/each}
			</ul>
			{#if selectedIds.length > 0}
				<section aria-label="Selected tracks">
					<h2>Selected</h2>
					<ol data-testid="queue-list">
						{#each selectedIds as id (id)}
							<li data-testid="queue-item" data-track-id={id} data-id={id}>
								{displayTitle(selectedTrack(id))}
							</li>
						{/each}
					</ol>
				</section>
			{/if}
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
			Usage metering lands with the usage-meter slice — no streamed bytes recorded yet.
		</p>
	</section>

	{#if overLimit}
		<p data-testid="plan-limit-notice" role="alert">
			You are over your plan limit — streaming is paused until the next cycle or an upgrade.
		</p>
	{/if}

	{#if currentTrack}
		<section aria-label="Player">
			<h2>Now playing</h2>
			<audio data-testid="audio-player" controls src=""></audio>
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
</style>
