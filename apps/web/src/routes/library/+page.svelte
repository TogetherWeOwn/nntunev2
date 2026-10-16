<script lang="ts">
	import { onMount } from 'svelte';
	import {
		broadcastAuthTransition,
		clearSnapshot,
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
	let authenticatedThisLoad = false;
	let activeRequest: AbortController | null = null;
	let requestGeneration = 0;

	const LIBRARY_REQUEST_TIMEOUT_MS = 10_000;

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
			if (response.status === 401 || response.status === 403) {
				authenticatedThisLoad = false;
				clearSnapshot(window.sessionStorage);
				clearSnapshot(window.localStorage);
				broadcastAuthTransition(window.localStorage);
				items = [];
				staleAt = null;
				loadError = 'You are signed out or do not have access to this library.';
				return;
			}
			if (!response.ok) throw new Error(`library request failed (${response.status})`);
			const payload = await Promise.race([response.json(), timeout]);
			const fresh = toLibraryItems(payload);
			if (fresh === null) throw new Error('library response has an invalid shape');
			authenticatedThisLoad = true;
			items = fresh;
			staleAt = null;
			saveSnapshot(window.sessionStorage, fresh);
		} catch {
			if (generation !== requestGeneration) return;
			// Do not trust a tab's snapshot until this page has verified its session.
			const snapshot = authenticatedThisLoad ? loadSnapshot(window.sessionStorage) : null;
			if (snapshot) {
				items = snapshot.items;
				staleAt = snapshot.savedAt;
			} else {
				items = [];
				staleAt = null;
				loadError = 'Library is not reachable and there is no saved copy yet.';
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
		clearSnapshot(window.sessionStorage);
		items = [];
		staleAt = null;
		loadError = 'Your session changed. Sign in again to see this library.';
		loading = false;
	}

	onMount(() => {
		window.addEventListener('storage', handleAuthTransition);
		// Discard the old shared localStorage snapshot from earlier versions.
		clearSnapshot(window.localStorage);
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

		{#if items.length === 0}
			<p role="status">Your library is empty.</p>
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
</main>
