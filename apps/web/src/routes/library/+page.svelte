<script lang="ts">
	import { onMount } from 'svelte';
	import {
		browserStorage,
		clearSnapshot,
		invalidateLibraryCache,
		LIBRARY_AUTH_TRANSITION_KEY,
		loadSnapshot,
		saveSnapshot,
		staleLabel,
		toLibraryItems,
		type LibraryItem
	} from '#lib/library/cache.js';

	let items: LibraryItem[] = $state([]);
	let staleAt: string | null = $state(null);
	let loading = $state(true);
	let loadError: string | null = $state(null);
	let libraryUnavailable = $state(false);
	let authenticatedThisLoad = false;
	let inMemorySnapshot: ReturnType<typeof saveSnapshot> | null = null;
	let activeRequest: AbortController | null = null;
	let requestGeneration = 0;

	const LIBRARY_REQUEST_TIMEOUT_MS = 10_000;

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

	async function refresh(fetchFn: typeof fetch = fetch): Promise<void> {
		if (activeRequest) return;
		const generation = ++requestGeneration;
		loading = true;
		loadError = null;
		libraryUnavailable = false;
		const controller = new AbortController();
		activeRequest = controller;
		let timeoutId: ReturnType<typeof setTimeout> | undefined;
		const timeout = new Promise<never>((_, reject) => {
			timeoutId = setTimeout(() => {
				controller.abort();
				reject(new Error('library request timed out'));
			}, LIBRARY_REQUEST_TIMEOUT_MS);
		});

		try {
			const response = await Promise.race([
				fetchFn('/api/library', { credentials: 'include', signal: controller.signal }),
				timeout
			]);
			if (generation !== requestGeneration) return;
			if (response.status === 404) {
				// The list endpoint has not landed yet. Do not treat a 404 as
				// session verification or expose a previously saved snapshot.
				authenticatedThisLoad = false;
				inMemorySnapshot = null;
				items = [];
				staleAt = null;
				libraryUnavailable = true;
				return;
			}
			if (response.status === 401 || response.status === 403) {
				authenticatedThisLoad = false;
				inMemorySnapshot = null;
				invalidateLibraryCache();
				items = [];
				staleAt = null;
				loadError = 'You are signed out or do not have access to this library.';
				return;
			}
			if (!response.ok) throw new Error(`library request failed (${response.status})`);
			const payload = await Promise.race([response.json(), timeout]);
			if (generation !== requestGeneration) return;
			const fresh = toLibraryItems(payload);
			if (fresh === null) throw new Error('library response has an invalid shape');
			authenticatedThisLoad = true;
			items = fresh;
			staleAt = null;
			const persistedSnapshot = saveSnapshot(browserStorage('session'), fresh);
			inMemorySnapshot = { ...persistedSnapshot, items: fresh };
		} catch {
			if (generation !== requestGeneration) return;
			// Do not trust a tab's snapshot until this page has verified its session.
			const snapshot = authenticatedThisLoad
				? (inMemorySnapshot ?? loadSnapshot(browserStorage('session')))
				: null;
			if (snapshot) {
				items = snapshot.items;
				staleAt = snapshot.savedAt;
			} else {
				items = [];
				staleAt = null;
				loadError = 'Library is not reachable right now.';
			}
		} finally {
			if (timeoutId !== undefined) clearTimeout(timeoutId);
			if (activeRequest === controller) {
				activeRequest = null;
				if (generation === requestGeneration) loading = false;
			}
		}
	}

	function handleAuthTransition(event: StorageEvent): void {
		if (event.key !== LIBRARY_AUTH_TRANSITION_KEY) return;
		requestGeneration += 1;
		activeRequest?.abort();
		activeRequest = null;
		authenticatedThisLoad = false;
		inMemorySnapshot = null;
		clearSnapshot(browserStorage('session'));
		items = [];
		staleAt = null;
		loadError = 'Your session changed. Sign in again to see this library.';
		loading = false;
	}

	onMount(() => {
		window.addEventListener('storage', handleAuthTransition);
		// Discard the old shared localStorage snapshot from earlier versions.
		clearSnapshot(browserStorage('local'));
		void refresh();
		return () => {
			requestGeneration += 1;
			activeRequest?.abort();
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
					<li data-testid="library-item" data-id={track.id}>
						{displayTitle(track)}
						{#if displayArtist(track)}
							<span> — {displayArtist(track)}</span>
						{/if}
					</li>
				{/each}
				</ul>
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
