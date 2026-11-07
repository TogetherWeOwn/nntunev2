<script lang="ts">
	import {
		browserStorage,
		invalidateLibraryCache,
		libraryItemKey,
		type LibraryItem
	} from '#lib/library/cache.js';
	import { favoritesKey, loadFavorites, selectFavoriteItems } from '#lib/library/favorites.js';
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

	const refresher = createLibraryRefresher({
		sessionStorage: () => browserStorage('session'),
		invalidateLibraryCache,
		onStateChange: (state) => {
			items = state.items;
			loading = state.loading;
			loadError = state.loadError;
			libraryUnavailable = state.libraryUnavailable;
			requiresSignIn = state.requiresSignIn;
		}
	});

	const session = browserStorage('session');
	const local = browserStorage('local');
	let favoriteIds = $state(loadFavorites(session, local));
	let recentQueries = $state(loadRecentFinds(session, local));

	$effect(() => {
		void refresher.refresh();
		return () => refresher.dispose();
	});

	// Another tab's write leaves the Liked count and recent list stale until
	// Home remounts; reload the changed list when its storage key changes
	// (a null key means the area was cleared, so reload both lists).
	$effect(() => {
		const onStorage = (event: StorageEvent): void => {
			if (event.key === null || event.key === favoritesKey(session)) {
				favoriteIds = loadFavorites(session, local);
			}
			if (event.key === null || event.key === recentFindsKey(session)) {
				recentQueries = loadRecentFinds(session, local);
			}
		};
		window.addEventListener('storage', onStorage);
		return () => window.removeEventListener('storage', onStorage);
	});

	const preview = $derived(items.slice(0, 5));
	const liked = $derived(selectFavoriteItems(items, favoriteIds));
</script>

<svelte:head>
	<title>Home — NNTune</title>
</svelte:head>

<main class="nt-wrap" id="main-content">
	<h1>Home</h1>

	{#if loading}
		<p role="status">Loading your library…</p>
	{:else if requiresSignIn}
		<p>
			Sign in to see your library. <a href="/login">Sign in</a>
		</p>
	{:else if loadError}
		<div role="alert">
			<p>Could not load your library: {loadError}</p>
			<button type="button" onclick={() => refresher.refresh()}>Retry</button>
		</div>
	{:else if libraryUnavailable}
		<p>Your library is not available yet.</p>
	{:else if items.length === 0}
		<p>
			Your library is empty. <a href="/library">Open Your Library</a> to add tracks.
		</p>
	{:else}
		<section aria-labelledby="home-library-heading">
			<h2 id="home-library-heading">From your library ({items.length})</h2>
			<ul>
				{#each preview as item (libraryItemKey(item))}
					<li>{displayTitle(item)}{displayArtist(item) ? ` — ${displayArtist(item)}` : ''}</li>
				{/each}
			</ul>
			<p><a href="/library">Open Your Library</a></p>
		</section>
	{/if}

	{#if liked.length > 0}
		<section aria-labelledby="home-liked-heading">
			<h2 id="home-liked-heading">Liked ({liked.length})</h2>
			<p><a href="/library/liked">Open Liked</a></p>
		</section>
	{/if}

	{#if recentQueries.length > 0}
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
