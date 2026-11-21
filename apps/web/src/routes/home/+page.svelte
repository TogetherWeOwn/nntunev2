<script lang="ts">
	import { onMount } from 'svelte';
	import {
		browserStorage,
		invalidateLibraryCache,
		libraryItemKey,
		type LibraryItem
	} from '#lib/library/cache.js';
	import { favoritesKey, loadFavorites, selectFavoriteItems } from '#lib/library/favorites.js';
	import { mountLibraryRefresher } from '#lib/library/mount-refresher.js';
	import { displayArtist, displayTitle } from '#lib/library/order.js';
	import { loadRecentFinds, recentFindsKey } from '#lib/library/recent-finds.js';
	import { createLibraryRefresher } from '#lib/library/refresh.js';

	// Home shows only the visitor's own data: their library snapshot, their
	// favorites and their recent searches. Everything loads client-side with
	// loading/error/empty states; nothing here fetches on the server.
	let items = $state<LibraryItem[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let libraryUnavailable = $state(false);
	let requiresSignIn = $state(false);
	let accessRefused = $state(false);

	const session = browserStorage('session');
	const local = browserStorage('local');
	let favoriteIds = $state(loadFavorites(session, local));
	let recentQueries = $state<string[]>([]);
	let readyRecentFindsKey: string | null = null;

	const refresher = createLibraryRefresher({
		sessionStorage: () => browserStorage('session'),
		invalidateLibraryCache,
		onStateChange: (state, update) => {
			items = state.items;
			loading = state.loading;
			loadError = state.loadError;
			libraryUnavailable = state.libraryUnavailable;
			requiresSignIn = state.requiresSignIn;
			accessRefused = update.authBoundary && !state.verificationSuspended && !state.requiresSignIn;
			if (update.authBoundary) {
				readyRecentFindsKey = null;
				recentQueries = [];
			}
			// Only a current successful listing may make its history scope ready.
			void update.runIfCurrent?.(() => {
				readyRecentFindsKey = recentFindsKey(session);
				recentQueries = loadRecentFinds(session, local);
			});
		}
	});

	onMount(() => {
		const unmountRefresher = mountLibraryRefresher(refresher);
		// A null storage key means the area was cleared.

		const onStorage = (event: StorageEvent): void => {
			if (event.key === null || event.key === favoritesKey(session)) {
				favoriteIds = loadFavorites(session, local);
			}
			if (
				readyRecentFindsKey !== null &&
				(event.key === null || event.key === readyRecentFindsKey) &&
				readyRecentFindsKey === recentFindsKey(session)
			) {
				recentQueries = loadRecentFinds(session, local);
			}
		};
		window.addEventListener('storage', onStorage);
		return () => {
			window.removeEventListener('storage', onStorage);
			unmountRefresher();
		};
	});

	const preview = $derived(items.slice(0, 5));
	const liked = $derived(selectFavoriteItems(items, favoriteIds));
</script>

<svelte:head>
	<title>Home — NNTune</title>
</svelte:head>

<main class="nt-wrap" id="main-content">
	<h1>Home</h1>

	<section aria-labelledby="home-discovery-heading">
		<h2 id="home-discovery-heading">Discover your library</h2>
		<p>
			This preview uses the same library catalogue as Search. It shows up to five tracks in
			catalogue order. Your account listening history is not used to select or rank them.
		</p>

		{#if loading}
			<p role="status">Loading your library…</p>
		{:else if requiresSignIn}
			<p>
				Sign in to see your library. <a href="/login">Sign in</a>
			</p>
		{:else if accessRefused}
			<p role="alert">You do not have access to this library.</p>
		{:else if loadError}
			<div role="alert">Could not load your library: {loadError}</div>
			<button type="button" onclick={() => refresher.refresh()}>Retry</button>
		{:else if libraryUnavailable}
			<p>Your library is not available yet.</p>
		{:else if items.length === 0}
			<p>
				Your library is empty. <a href="/library">Open Your Library</a> to add tracks.
			</p>
		{:else}
			<h3>From your library ({items.length})</h3>
			<ul>
				{#each preview as item (libraryItemKey(item))}
					<li>{displayTitle(item)}{displayArtist(item) ? ` — ${displayArtist(item)}` : ''}</li>
				{/each}
			</ul>
			<p><a href="/library">Open Your Library</a></p>
			<p><a href="/search">Search your library</a></p>
		{/if}
	</section>

	{#if liked.length > 0}
		<section aria-labelledby="home-liked-heading">
			<h2 id="home-liked-heading">Liked ({liked.length})</h2>
			<p><a href="/library/liked">Open Liked</a></p>
		</section>
	{/if}

	{#if !loading && !loadError && !libraryUnavailable && recentQueries.length > 0}
		<section aria-labelledby="home-recent-heading">
			<h2 id="home-recent-heading">Recent searches</h2>
			<ul>
				{#each recentQueries as query (query)}
					<li><a href={`/search?q=${encodeURIComponent(query)}`}>{query}</a></li>
				{/each}
			</ul>
		</section>
	{/if}
</main>
