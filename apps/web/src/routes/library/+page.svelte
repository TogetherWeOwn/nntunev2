<script lang="ts">
	import { onMount } from 'svelte';
	import {
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
		loading = true;
		loadError = null;
		try {
			const response = await fetchFn('/api/library', { credentials: 'include' });
			if (!response.ok) throw new Error(`library request failed (${response.status})`);
			const fresh = toLibraryItems(await response.json());
			items = fresh;
			staleAt = null;
			saveSnapshot(window.localStorage, fresh);
		} catch {
			// API unreachable (tunnel down, Access hiccup, airplane wifi):
			// render the last-good snapshot with an explicit staleness badge
			// instead of a blank screen.
			const snapshot = loadSnapshot(window.localStorage);
			if (snapshot) {
				items = snapshot.items;
				staleAt = snapshot.savedAt;
			} else {
				items = [];
				staleAt = null;
				loadError = 'Library is not reachable and there is no saved copy yet.';
			}
		} finally {
			loading = false;
		}
	}

	onMount(() => {
		void refresh();
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

		{#if staleAt}
			<button class="nt-button nt-button--ghost" type="button" onclick={() => void refresh()}>
				Retry
			</button>
		{/if}
	{/if}
</main>
