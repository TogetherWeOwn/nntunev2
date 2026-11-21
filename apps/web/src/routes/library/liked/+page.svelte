<script lang="ts">
	import { onMount } from 'svelte';
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
		selectFavoriteItems,
		updateStoredFavorites
	} from '#lib/library/favorites.js';
	import { restoreFavoriteId } from '#lib/library/favorites-order.js';
	import { mountLibraryRefresher } from '#lib/library/mount-refresher.js';
	import { displayArtist, displayTitle } from '#lib/library/order.js';
	import { recentPlayTitles } from '#lib/library/recent-plays.js';
	import { createLibraryRefresher, type LibraryRefreshUpdate } from '#lib/library/refresh.js';
	import { pushToast } from '#lib/shell/toasts.js';

	// Liked is the visitor's starred subset of their own library: loaded
	// rows projected through the device-local favorites for this auth
	// context. Removing a favorite persists immediately and offers Undo.
	let items = $state<LibraryItem[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let libraryUnavailable = $state(false);
	let requiresSignIn = $state(false);

	let libraryBinding = $state<string>();
	let deviceScope = $state<string>();
	const refresher = createLibraryRefresher({
		sessionStorage: () => browserStorage('session'),
		invalidateLibraryCache,
		onStateChange: (state, { isCurrent, authoritative, authBoundary, sessionBinding, deviceScope: verifiedScope }) => {
			if (!isCurrent()) return;
			const key = favoritesKey(session);
			if ((authBoundary && !state.verificationSuspended) || (sessionBinding && favoriteVisitKey !== key)) {
				pendingChanges.clear();
				favoriteVisitKey = null;
				favoriteIds = [];
			}
			libraryBinding = sessionBinding;
			deviceScope = verifiedScope;
			if (authoritative && !authBoundary && !state.libraryUnavailable) {
				favoriteVisitKey = key;
				favoriteIds = currentFavorites();
			}
			items = state.items;
			loading = state.loading;
			loadError = state.loadError;
			libraryUnavailable = state.libraryUnavailable;
			requiresSignIn = state.requiresSignIn;
		}
	});

	onMount(() => mountLibraryRefresher(refresher));

	const session = browserStorage('session');
	const local = browserStorage('local');
	let favoriteIds = $state<string[]>(loadFavorites(session, local));
	let favoriteVisitKey = favoritesKey(session);

	const liked = $derived(selectFavoriteItems(items, favoriteIds));

	const titles = $derived(recentPlayTitles(items));

	type FavoriteChange = (ids: readonly string[]) => readonly string[];
	const pendingChanges = new Map<string, FavoriteChange>();

	async function retireDeviceIds(scope: string, ids: readonly string[], verify: () => Promise<boolean>,
		run: NonNullable<LibraryRefreshUpdate['runIfCurrent']>): Promise<boolean> {
		const pending = new Map(pendingChanges);
		const remaining = await updateStoredFavorites(session, local, scope,
			(stored) => stored.filter((id) => !ids.includes(id)), verify, run);
		return !!remaining && !!(await run(() => {
			for (const id of ids) if (pendingChanges.get(id) === pending.get(id)) pendingChanges.delete(id);
			favoriteIds = currentFavorites();
			return true;
		}));
	}

	function applyChanges(ids: readonly string[], changes = pendingChanges): string[] {
		for (const change of changes.values()) ids = change(ids);
		return [...ids];
	}

	function currentFavorites(): string[] {
		return applyChanges(loadFavorites(session, local, favoriteIds));
	}

	async function persistChange(id: string, change: FavoriteChange): Promise<void> {
		const scope = favoriteVisitKey;
		if (!scope || scope !== favoritesKey(session)) return;
		pendingChanges.delete(id);
		pendingChanges.set(id, change);
		favoriteIds = currentFavorites();
		let changes = pendingChanges;
		const stored = await updateStoredFavorites(session, local, scope, (ids) => {
			changes = new Map(pendingChanges);
			return applyChanges(ids, changes);
		});
		if (!stored || scope !== favoriteVisitKey) return;
		for (const [id, saved] of changes) if (pendingChanges.get(id) === saved) pendingChanges.delete(id);
		favoriteIds = currentFavorites();
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
		const key = favoriteVisitKey;
		void persistChange(id, (ids) => ids.filter((entry) => entry !== id));
		pushToast(`Removed “${title}” from Liked.`, {
			action: {
				label: 'Undo',
				run: () => {
					if (!key || key !== favoriteVisitKey) return;
					// Restore only this track against the stored list, so a
					// navigation or another tab's save is never overwritten,
					// and anchor on the surviving successor so adjacent Undos
					// restore the original order in either sequence.
					void persistChange(id, (ids) => restoreFavoriteId(ids, id, anchor));
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
	{#if !loading && !requiresSignIn && !loadError}
		<AccountLikes {titles} {libraryBinding} {deviceScope} readDeviceIds={currentFavorites} {retireDeviceIds}
			deviceCount={deviceScope ? favoriteIds.length : 0} onSessionChanged={refresher.handleLocalAuthTransition} />
	{/if}
</main>
