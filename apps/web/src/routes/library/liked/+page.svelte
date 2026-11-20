<script lang="ts">
	import { onMount } from 'svelte';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import AccountLikes from '#lib/library/AccountLikes.svelte';
	import {
		browserStorage,
		invalidateLibraryCache,
		libraryItemKey,
		type LibraryItem
	} from '#lib/library/cache.js';
	import {
		favoritesKey,
		loadFavorites,
		retireStoredFavorites,
		saveFavorites,
		selectFavoriteItems
	} from '#lib/library/favorites.js';
	import { restoreFavoriteId } from '#lib/library/favorites-order.js';
	import { displayArtist, displayTitle } from '#lib/library/order.js';
	import { recentPlayTitles } from '#lib/library/recent-plays.js';
	import { createLibraryRefresher } from '#lib/library/refresh.js';
	import { pushToast } from '#lib/shell/toasts.js';

	let items = $state<LibraryItem[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let libraryUnavailable = $state(false);
	let requiresSignIn = $state(false);
	let deviceScope = $state<string | null>(null);
	const session = browserStorage('session');
	const local = browserStorage('local');
	let favoriteIds = $state<string[]>([]);
	const liked = $derived(selectFavoriteItems(items, favoriteIds));
	const titles = $derived(recentPlayTitles(items));

	type FavoriteChange = (ids: readonly string[]) => readonly string[];
	const pendingChanges = new Map<string, FavoriteChange>();

	const refresher = createLibraryRefresher({
		sessionStorage: () => session,
		invalidateLibraryCache,
		onStateChange: (state, { authoritative, authBoundary }) => {
			items = state.items;
			loading = state.loading;
			loadError = state.loadError;
			libraryUnavailable = state.libraryUnavailable;
			requiresSignIn = state.requiresSignIn;
			if (authBoundary) {
				deviceScope = null;
				pendingChanges.clear();
				favoriteIds = [];
			} else if (authoritative && !loadError && !libraryUnavailable) {
				deviceScope = favoritesKey(session);
				favoriteIds = currentFavorites();
			}
		}
	});

	onMount(() => {
		const unsubscribe = subscribeLocalAuthTransition(refresher.handleLocalAuthTransition);
		window.addEventListener('storage', refresher.handleAuthTransition);
		void refresher.refresh();
		return () => {
			unsubscribe();
			window.removeEventListener('storage', refresher.handleAuthTransition);
			refresher.dispose();
		};
	});

	function readDeviceIds(): string[] {
		return deviceScope && deviceScope === favoritesKey(session) ? currentFavorites() : [];
	}

	function retireDeviceIds(ids: readonly string[]): void {
		if (!deviceScope || !retireStoredFavorites(session, local, deviceScope, ids)) return;
		for (const id of ids) pendingChanges.delete(id);
		favoriteIds = currentFavorites();
	}

	function currentFavorites(): string[] {
		// Retry unsaved per-track changes against fresh storage, preserving
		// both this visit's edits and additions from another tab.
		let ids: readonly string[] = loadFavorites(session, local, favoriteIds);
		for (const change of pendingChanges.values()) ids = change(ids);
		return [...ids];
	}

	function persistChange(id: string, change: FavoriteChange): void {
		if (!deviceScope || deviceScope !== favoritesKey(session)) return;
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
	{:else if liked.length > 0}
		<h2>Device favorites</h2>
		<ul>
			{#each liked as item (libraryItemKey(item))}
				<li>
					{displayTitle(item)}{displayArtist(item) ? ` — ${displayArtist(item)}` : ''}
					<button type="button" onclick={() => removeFavorite(item.id, displayTitle(item))}>
						Remove {displayTitle(item)} from Liked
					</button>
				</li>
			{/each}
		</ul>
	{/if}
	{#if !requiresSignIn}
		<AccountLikes
			{titles}
			deviceCount={!loading && !loadError && !libraryUnavailable && deviceScope ? favoriteIds.length : 0}
			{readDeviceIds}
			{retireDeviceIds}
		/>
	{/if}
</main>
