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

	type FavoriteChange = (ids: readonly string[]) => readonly string[];
	const pendingChanges = new Map<string, FavoriteChange>();

	function currentFavorites(): string[] {
		// Retry unsaved per-track changes against fresh storage, preserving
		// both this visit's edits and additions from another tab.
		let ids: readonly string[] = loadFavorites(session, local, favoriteIds);
		for (const change of pendingChanges.values()) ids = change(ids);
		return [...ids];
	}

	function persistChange(id: string, change: FavoriteChange): void {
		pendingChanges.delete(id);
		pendingChanges.set(id, change);
		favoriteIds = currentFavorites();
		if (saveFavorites(session, local, favoriteIds)) pendingChanges.clear();
	}

	function removeFavorite(id: string, title: string): void {
		// A missing stored id is an already-completed removal, not evidence
		// that storage is stale. Reconcile the rendered list without saving.
		const previous = currentFavorites();
		const previousIndex = previous.indexOf(id);
		if (previousIndex < 0) {
			favoriteIds = previous;
			return;
		}
		const anchor = {
			previousIndex,
			predecessor: previousIndex > 0 ? previous[previousIndex - 1] : null,
			successor: previousIndex < previous.length - 1 ? previous[previousIndex + 1] : null
		};
		persistChange(id, (ids) => ids.filter((entry) => entry !== id));
		pushToast(`Removed “${title}” from Liked.`, {
			action: {
				label: 'Undo',
				run: () => {
					// Restore only this track against the stored list, so a
					// navigation or another tab's save is never overwritten,
					// and anchor on the surviving successor so adjacent Undos
					// restore the original order in either sequence.
					persistChange(id, (ids) => restoreFavoriteId(ids, id, anchor));
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
