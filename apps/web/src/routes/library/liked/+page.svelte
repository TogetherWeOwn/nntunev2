<script lang="ts">
	import {
		browserStorage,
		invalidateLibraryCache,
		type LibraryItem
	} from '#lib/library/cache.js';
	import {
		loadFavorites,
		saveFavorites,
		selectFavoriteItems
	} from '#lib/library/favorites.js';
	import { restoreFavoriteId } from '#lib/library/favorites-order.js';
	import { displayArtist, displayTitle } from '#lib/library/order.js';
	import { createLibraryRefresher } from '#lib/library/refresh.js';
	import { pushToast } from '#lib/shell/toasts.js';

	// Liked is the visitor's starred subset of their own library: loaded
	// rows projected through the device-local favorites for this auth
	// context. Removing a favorite persists immediately and offers Undo.
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

	$effect(() => {
		void refresher.refresh();
		return () => refresher.dispose();
	});

	const session = browserStorage('session');
	const local = browserStorage('local');
	let favoriteIds = $state<string[]>(loadFavorites(session, local));

	const liked = $derived(selectFavoriteItems(items, favoriteIds));

	function persist(ids: string[]): void {
		favoriteIds = ids;
		saveFavorites(session, local, ids);
	}

	function removeFavorite(id: string, title: string): void {
		// Re-read storage so a favorite added in another tab is not dropped
		// by this removal; fall back to component state when storage is
		// stale (it lacks the id this list just rendered).
		const stored = loadFavorites(session, local);
		const previous = stored.includes(id) ? stored : [...favoriteIds];
		const previousIndex = previous.indexOf(id);
		if (previousIndex < 0) return;
		const anchor = {
			previousIndex,
			predecessor: previousIndex > 0 ? previous[previousIndex - 1] : null,
			successor: previousIndex < previous.length - 1 ? previous[previousIndex + 1] : null
		};
		persist(previous.filter((entry) => entry !== id));
		pushToast(`Removed “${title}” from Liked.`, {
			action: {
				label: 'Undo',
				run: () => {
					// Restore only this track against the stored list, so a
					// navigation or another tab's save is never overwritten,
					// and anchor on the surviving successor so adjacent Undos
					// restore the original order in either sequence.
					const current = loadFavorites(session, local);
					const next = restoreFavoriteId(current, id, anchor);
					if (next !== current) persist([...next]);
				}
			}
		});
	}
</script>

<svelte:head>
	<title>Liked — NNTune</title>
</svelte:head>

<main class="nt-wrap" id="main-content">
	<h1>Liked</h1>
	<p><a href="/library">Back to Your Library</a></p>

	{#if loading}
		<p role="status">Loading your liked tracks…</p>
	{:else if requiresSignIn}
		<p>
			Sign in to see your liked tracks. <a href="/login">Sign in</a>
		</p>
	{:else if loadError}
		<div role="alert">
			<p>Could not load your library: {loadError}</p>
			<button type="button" onclick={() => refresher.refresh()}>Retry</button>
		</div>
	{:else if libraryUnavailable}
		<p>Your library is not available yet.</p>
	{:else if liked.length === 0}
		<p>No liked tracks yet. Star tracks in Your Library to find them here.</p>
	{:else}
		<ul>
			{#each liked as item (item.id)}
				<li>
					{displayTitle(item)}{displayArtist(item) ? ` — ${displayArtist(item)}` : ''}
					<button type="button" onclick={() => removeFavorite(item.id, displayTitle(item))}>
						Remove {displayTitle(item)} from Liked
					</button>
				</li>
			{/each}
		</ul>
	{/if}
</main>
