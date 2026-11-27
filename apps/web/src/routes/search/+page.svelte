<script lang="ts">
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import { _maxQueryLength, _normalizeQuery } from './+page.js';
	import type { PageProps } from './$types';
	import {
		browserStorage,
		invalidateLibraryCache,
		libraryItemKey,
		type LibraryItem
	} from '#lib/library/cache.js';
	import { filterLibraryItems } from '#lib/library/filter.js';
	import { mountLibraryRefresher } from '#lib/library/mount-refresher.js';
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

	onMount(() => mountLibraryRefresher(refresher));

	// The applied query lives in the URL (`?q=`) so the top-bar search and
	// this page share one destination. The draft is the field's text; submit
	// applies it by navigating. An external navigation (the top bar) syncs
	// the draft — typing never touches the URL, so it never clobbers input.
	let { data }: PageProps = $props();
	let draft = $derived(data.query ?? data.recoveryDraft);
	let invalidDraft = $state(false);

	$effect(() => {
		if (data.query !== null) invalidDraft = false;
	});

	const results = $derived(data.query ? filterLibraryItems(items, data.query) : []);
	const statusMessage = $derived.by(() => {
		if (loading) return 'Loading your library…';
		if (requiresSignIn || loadError || libraryUnavailable || !data.query) return '';
		const count = results.length;
		if (count === 0) return `No results for “${data.query}”.`;
		return `${count} ${count === 1 ? 'result' : 'results'} for “${data.query}”.`;
	});

	function onSubmit(event: SubmitEvent): void {
		event.preventDefault();
		const query = _normalizeQuery(draft);
		invalidDraft = query === null;
		if (query === null) return;
		goto(query ? `/search?${new URLSearchParams({ q: query })}` : '/search');
	}
</script>

<svelte:head>
	<title>Search — NNTune</title>
</svelte:head>

<main class="nt-wrap" id="main-content">
	<h1>Search</h1>

	<form role="search" aria-labelledby="library-search-label" onsubmit={onSubmit}>
		<label id="library-search-label" for="search-field">Search your library</label>
		<input
			id="search-field"
			name="q"
			type="search"
			autocomplete="off"
			maxlength={_maxQueryLength}
			aria-invalid={data.query === null || invalidDraft}
			aria-describedby={data.query === null || invalidDraft ? 'search-query-error' : undefined}
			bind:value={draft}
			oninput={() => (invalidDraft = false)}
		/>
		<button type="submit">Search</button>
	</form>
	{#if data.query === null || invalidDraft}
		<p id="search-query-error" role="alert">
			{#if data.draftTruncated}Only the start of your search is shown. {/if}
			Enter a valid search with up to {_maxQueryLength} characters.
		</p>
	{/if}

	<p role="status" aria-atomic="true" class:nt-visually-hidden={!statusMessage}>{statusMessage}</p>
	{#if !loading}
		{#if requiresSignIn}
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
		{:else if data.query === ''}
			<p>Type above to search your library by title or artist.</p>
		{:else if results.length > 0}
			<ul>
				{#each results as item (libraryItemKey(item))}
					<li>{displayTitle(item)}{displayArtist(item) ? ` — ${displayArtist(item)}` : ''}</li>
				{/each}
			</ul>
		{/if}
	{/if}
</main>

<style>
	.nt-visually-hidden {
		position: absolute;
		width: 1px;
		height: 1px;
		margin: -1px;
		padding: 0;
		overflow: hidden;
		clip-path: inset(50%);
		white-space: nowrap;
		border: 0;
	}
</style>
