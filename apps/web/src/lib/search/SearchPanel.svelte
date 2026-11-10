<script lang="ts">
	import { onMount } from 'svelte';
	import { beforeNavigate, goto } from '$app/navigation';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import { browserStorage, invalidateLibraryCache, LIBRARY_AUTH_TRANSITION_KEY, type LibraryItem } from '#lib/library/cache.js';
	import { createLibraryRefresher } from '#lib/library/refresh.js';
	import { clearRecentFindsAtAuthBoundary, loadRecentFinds, recordRecentFind, saveRecentFinds } from '#lib/library/recent-finds.js';
	import { searchCatalogue, type CatalogueHit } from './catalogue.js';
	import { mergeSearchResults, SEARCH_CATEGORIES, searchHref, type SearchCategory } from './results.js';

	let { query = '', category = 'all', compact = false, onselect }: {
		query?: string; category?: SearchCategory; compact?: boolean; onselect?: () => void;
	} = $props();
	const labels: Record<SearchCategory, string> = {
		all: 'All', tracks: 'Songs', artists: 'Artists', albums: 'Albums', playlists: 'Playlists', videos: 'Videos'
	};
	let draft = $state('');
	let items = $state<LibraryItem[]>([]);
	let hits = $state<CatalogueHit[]>([]);
	let recents = $state<readonly string[]>([]);
	let recentsReady = false;
	let libraryLoading = $state(true);
	let requiresSignIn = $state(false);
	let libraryError = $state(false);
	let staleAt = $state<string | null>(null);
	let catalogueLoading = $state(false);
	let catalogueError = $state(false);
	let catalogueBlocked = $state(false);
	let retry = $state(0);
	let request: AbortController | null = null;
	let navigationTimer: ReturnType<typeof setTimeout> | undefined;
	beforeNavigate(() => clearTimeout(navigationTimer));
	let resultRoot = $state<HTMLDivElement>();
	let input: HTMLInputElement | undefined;

	function resetAtAuthBoundary(): void {
		request?.abort();
		catalogueLoading = false;
		catalogueError = false;
		catalogueBlocked = true;
		clearRecentFindsAtAuthBoundary(browserStorage('session'), browserStorage('local'));
		recents = [];
		recentsReady = false;
		hits = [];
		items = [];
		clearTimeout(navigationTimer);
	}
	const refresher = createLibraryRefresher({
		sessionStorage: () => browserStorage('session'),
		invalidateLibraryCache: () => { resetAtAuthBoundary(); invalidateLibraryCache(); },
		onStateChange: (state) => {
			items = state.items;
			libraryLoading = state.loading;
			requiresSignIn = state.requiresSignIn;
			libraryError = !!state.loadError || state.libraryUnavailable;
			staleAt = state.staleAt;
			if (!state.loading && !state.requiresSignIn && !state.loadError && !state.libraryUnavailable) {
				catalogueBlocked = false;
				if (!recentsReady) {
					recents = loadRecentFinds(browserStorage('session'), browserStorage('local'));
					recentsReady = true;
				}
			}
		}
	});

	onMount(() => {
		const onAuth = () => { resetAtAuthBoundary(); refresher.handleLocalAuthTransition(); };
		const onStorage = (event: StorageEvent) => {
			if (event.key === LIBRARY_AUTH_TRANSITION_KEY) onAuth();
		};
		const unsubscribe = subscribeLocalAuthTransition(onAuth);
		window.addEventListener('storage', onStorage);
		void refresher.refresh();
		if (compact) input?.focus();
		return () => {
			unsubscribe();
			window.removeEventListener('storage', onStorage);
			clearTimeout(navigationTimer);
			request?.abort();
			refresher.dispose();
		};
	});

	$effect(() => { draft = query; });
	const effectiveQuery = $derived((compact ? draft : query).trim());
	const results = $derived(mergeSearchResults(items, hits, effectiveQuery));
	const filtered = $derived(category === 'all' ? results : results.filter((result) => result.category === category));
	const visible = $derived(compact ? filtered.slice(0, 5) : filtered);
	const suggestions = $derived(recents.filter((entry) => entry !== effectiveQuery && entry.toLowerCase().includes(effectiveQuery.toLowerCase())));

	$effect(() => {
		const value = effectiveQuery;
		void retry;
		hits = [];
		catalogueError = false;
		if (requiresSignIn || catalogueBlocked || [...value].length < 2 || [...value].length > 100) {
			catalogueLoading = false;
			return;
		}
		const controller = new AbortController();
		request = controller;
		catalogueLoading = true;
		const timer = setTimeout(async () => {
			try {
				const response = await searchCatalogue(PUBLIC_API_BASE_URL, value, controller.signal);
				if (!controller.signal.aborted) hits = response;
			} catch {
				if (!controller.signal.aborted) catalogueError = true;
			} finally {
				if (!controller.signal.aborted) catalogueLoading = false;
			}
		}, compact ? 200 : 0);
		return () => { clearTimeout(timer); controller.abort(); };
	});

	function remember(value: string): void {
		if (!recentsReady || requiresSignIn) return;
		recents = recordRecentFind(recents, value);
		saveRecentFinds(browserStorage('session'), browserStorage('local'), recents);
	}
	function removeRecent(value?: string): void {
		recents = value === undefined ? [] : recents.filter((entry) => entry !== value);
		saveRecentFinds(browserStorage('session'), browserStorage('local'), recents);
	}
	function navigate(value: string, replaceState = false): void {
		void goto(searchHref(value, category), { replace: replaceState, reset: !replaceState });
	}
	function onInput(): void {
		clearTimeout(navigationTimer);
		if (!compact) navigationTimer = setTimeout(() => navigate(draft, true), 200);
	}
	function submit(event: SubmitEvent): void {
		event.preventDefault();
		clearTimeout(navigationTimer);
		remember(draft);
		navigate(draft);
		onselect?.();
	}
	function inputKeydown(event: KeyboardEvent): void {
		if (event.key === 'ArrowDown') {
			const first = resultRoot?.querySelector<HTMLAnchorElement>('[data-search-result]');
			if (first) { event.preventDefault(); first.focus(); }
		} else if (event.key === 'Escape' && !compact) {
			clearTimeout(navigationTimer);
			draft = '';
			navigate('');
			input?.blur();
		}
	}
	function resultKeydown(event: KeyboardEvent): void {
		if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
		const links = [...(resultRoot?.querySelectorAll<HTMLAnchorElement>('[data-search-result]') ?? [])];
		const index = links.indexOf(document.activeElement as HTMLAnchorElement);
		if (index < 0) return;
		event.preventDefault();
		if (event.key === 'ArrowUp' && index === 0) input?.focus();
		else links[(index + (event.key === 'ArrowDown' ? 1 : -1) + links.length) % links.length]?.focus();
	}
</script>

<form role="search" onsubmit={submit}>
	<label for={compact ? 'quick-search-field' : 'search-field'}>Search music</label>
	<div class="search-field-row">
		<input id={compact ? 'quick-search-field' : 'search-field'} type="search" name="q" maxlength="100"
			autocomplete="off" bind:this={input} bind:value={draft} oninput={onInput} onkeydown={inputKeydown} />
		<button class="nt-button" type="submit">Search</button>
	</div>
</form>

{#if requiresSignIn}
	<p>Sign in to search your library. <a href="/login">Sign in</a></p>
{:else}
	{#if !compact && effectiveQuery}
		<nav class="search-chips" aria-label="Search types">
			{#each SEARCH_CATEGORIES as type}
				<a class="nt-button nt-button--ghost" href={searchHref(effectiveQuery, type)} aria-current={category === type ? 'page' : undefined}>{labels[type]}</a>
			{/each}
		</nav>
	{/if}
	{#if libraryLoading}<p role="status">Loading your library…</p>{/if}
	{#if libraryError}
		<div role="status"><p>Your library is not available. Catalogue results may be incomplete.</p>
			<button type="button" class="nt-button nt-button--ghost" onclick={() => refresher.refresh()}>Retry library</button></div>
	{/if}
	{#if staleAt}<p>Showing saved library copy from {staleAt}.</p>{/if}
	{#if catalogueError}
		<div role="status"><p>Catalogue search is unavailable. Showing library matches only.</p>
			<button type="button" class="nt-button nt-button--ghost" onclick={() => retry++}>Retry catalogue</button></div>
	{/if}
	{#if !effectiveQuery || (compact && suggestions.length > 0)}
		<section aria-label="Recent searches">
			<h2>{effectiveQuery ? 'Suggestions' : 'Recent searches'}</h2>
			<p>Saved on this device.</p>
			{#if recents.length > 0}
				<ul>
					{#each (effectiveQuery ? suggestions : recents) as recent}
						<li><a href={searchHref(recent)} onclick={() => { remember(recent); onselect?.(); }}>{recent}</a>
							<button type="button" class="nt-button nt-button--ghost" aria-label={`Remove ${recent} from recent searches`} onclick={() => removeRecent(recent)}>Remove</button></li>
					{/each}
				</ul>
				<button type="button" class="nt-button nt-button--ghost" onclick={() => removeRecent()}>Clear recent searches</button>
			{:else}<p>Your searches will appear here.</p>{/if}
		</section>
	{/if}
	{#if effectiveQuery}
		{#if catalogueLoading}<p role="status">Searching the catalogue…</p>{/if}
		{#if !libraryLoading}
			<p role="status">{filtered.length} {filtered.length === 1 ? 'result' : 'results'} for “{effectiveQuery}”.</p>
		{/if}
		{#if filtered.length === 0 && !libraryLoading && !catalogueLoading}
			<p>No {category === 'all' ? 'music' : labels[category].toLowerCase()} found for “{effectiveQuery}”. Try another spelling or search all music.</p>
			<a href={searchHref(effectiveQuery)}>All results</a> · <a href="/search">Clear search</a>
		{/if}
		<div bind:this={resultRoot} role="group" aria-label="Search results">
			{#if !compact && category === 'all' && visible.length > 0}
				<section class="top-result" aria-label="Top result">
					<h2>Top result</h2>
					<a data-search-result onkeydown={resultKeydown} href={visible[0].href} onclick={() => remember(effectiveQuery)}>{visible[0].label}</a>
					{#if visible[0].inLibrary}<p>In your library</p>{/if}
				</section>
			{/if}
			{#each (compact ? [category] : SEARCH_CATEGORIES.filter((type) => type !== 'all' && (category === 'all' || type === category))) as group}
				{@const rows = compact ? visible : visible.filter((result) => result.category === group)}
				{#if rows.length > 0}
					<section aria-label={compact ? 'Top hits' : labels[group]}>
						<h2>{compact ? 'Top hits' : labels[group]}</h2>
						<ul class="search-results">
							{#each rows as result (result.key)}
								<li><a data-search-result onkeydown={resultKeydown} href={result.href} onclick={() => { remember(effectiveQuery); onselect?.(); }}>{result.label}{result.artist ? ` — ${result.artist}` : ''}</a>
									{#if result.inLibrary}<span class="availability">In your library</span>{/if}</li>
							{/each}
						</ul>
					</section>
				{/if}
			{/each}
		</div>
		{#if compact}<a href={searchHref(effectiveQuery)} onclick={() => { remember(effectiveQuery); onselect?.(); }}>Search for “{effectiveQuery}”</a>{/if}
	{:else}<p>Search by song, album or artist. Use ↓ to move into results.</p>{/if}
{/if}

<style>
	.search-field-row, .search-chips, li { display: flex; flex-wrap: wrap; align-items: center; gap: var(--nt-space-2); }
	.search-field-row input { flex: 1; min-width: 0; padding: var(--nt-space-2); border: 1px solid var(--nt-border); border-radius: var(--nt-radius-pill); background: var(--nt-bg-raised); color: var(--nt-fg); font: inherit; }
	.search-chips { margin-block: var(--nt-space-3); }
	.search-chips a { border-radius: var(--nt-radius-pill); }
	.search-chips a[aria-current='page'] { background: var(--nt-accent); color: var(--nt-accent-fg); }
	ul { list-style: none; padding: 0; }
	li { padding-block: var(--nt-space-2); }
	.search-results li { border-bottom: 1px solid var(--nt-border); }
	.availability { color: var(--nt-fg-muted); font-size: var(--nt-text-sm); }
	.top-result { padding: var(--nt-space-3); background: var(--nt-bg-raised); border-radius: var(--nt-radius); }
	a { overflow-wrap: anywhere; }
</style>
