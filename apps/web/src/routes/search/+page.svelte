<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import {
		browserStorage,
		invalidateLibraryCache,
		libraryItemKey,
		type LibraryItem
	} from '#lib/library/cache.js';
	import { filterLibraryItems } from '#lib/library/filter.js';
	import { displayArtist, displayTitle } from '#lib/library/order.js';
	import { createLibraryRefresher } from '#lib/library/refresh.js';

	// Search filters only the visitor's own library rows: the same
	// title/artist matching as the library find field, over the same
	// refresh state machine (loading, sign-in, error, unavailable).
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

	// The applied query lives in the URL (`?q=`) so the top-bar search and
	// this page share one destination. The draft is the field's text; submit
	// applies it by navigating. An external navigation (the top bar) syncs
	// the draft — typing never touches the URL, so it never clobbers input.
	const applied = $derived(page.url.searchParams.get('q') ?? '');
	let draft = $state('');

	$effect(() => {
		draft = applied;
	});

	const trimmed = $derived(applied.trim());
	const results = $derived(filterLibraryItems(items, trimmed));

	function onSubmit(event: SubmitEvent): void {
		event.preventDefault();
		const query = draft.trim();
		goto(query ? `/search?q=${encodeURIComponent(query)}` : '/search');
	}
</script>

<svelte:head>
	<title>Search — NNTune</title>
</svelte:head>

<main class="nt-wrap" id="main-content">
	<h1>Search</h1>

	<form role="search" onsubmit={onSubmit}>
		<label for="search-field">Search your library</label>
		<input
			id="search-field"
			name="q"
			type="search"
			autocomplete="off"
			bind:value={draft}
		/>
		<button type="submit">Search</button>
	</form>

	{#if loading}
		<p role="status">Loading your library…</p>
	{:else if requiresSignIn}
		<p>
			Sign in to search your library. <a href="/login">Sign in</a>
		</p>
	{:else if loadError}
		<div role="alert">
			<p>Could not load your library: {loadError}</p>
			<button type="button" onclick={() => refresher.refresh()}>Retry</button>
		</div>
	{:else if libraryUnavailable}
		<p>Your library is not available yet.</p>
	{:else if trimmed.length === 0}
		<p>Type above to search your library by title or artist.</p>
	{:else if results.length === 0}
		<p role="status">No results for “{trimmed}”.</p>
	{:else}
		<p role="status">
			{results.length} {results.length === 1 ? 'result' : 'results'} for “{trimmed}”.
		</p>
		<ul>
			{#each results as item (libraryItemKey(item))}
				<li>{displayTitle(item)}{displayArtist(item) ? ` — ${displayArtist(item)}` : ''}</li>
			{/each}
		</ul>
	{/if}
</main>
