<script lang="ts">
	import { onMount } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient, ApiError } from '#lib/api/client.js';
	import Artwork from '#lib/player/Artwork.svelte';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import { invalidateLibraryCache, LIBRARY_AUTH_TRANSITION_KEY } from './cache.js';
	import { displayCollections, loadCollections, type CollectionKind, type CollectionSort, type LibraryCollection } from './collections.js';

	let { kind, density }: { kind: CollectionKind; density: 'comfortable' | 'compact' } = $props();
	let items = $state<LibraryCollection[]>([]);
	let query = $state('');
	let sort = $state<CollectionSort>('original');
	let descending = $state(false);
	let view = $state<'list' | 'grid'>('list');
	let loading = $state(true);
	let error = $state('');
	let requiresSignIn = $state(false);
	let generation = 0;
	let activeRequest: AbortController | null = null;
	const displayed = $derived(displayCollections(items, query, sort, descending));

	function cancel(): void {
		generation += 1;
		activeRequest?.abort();
		activeRequest = null;
	}

	function clearAccount(): void {
		cancel();
		items = [];
		query = '';
		sort = 'original';
		descending = false;
		loading = false;
		requiresSignIn = true;
		error = 'Your session changed. Sign in again to load your library.';
	}

	async function refresh(selectedKind: CollectionKind): Promise<void> {
		cancel();
		const requestGeneration = generation;
		const controller = new AbortController();
		activeRequest = controller;
		items = [];
		loading = true;
		error = '';
		requiresSignIn = false;
		try {
			const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, signal: controller.signal });
			const rows = await loadCollections(client, selectedKind);
			if (requestGeneration !== generation || controller.signal.aborted) return;
			items = rows;
		} catch (cause) {
			if (requestGeneration !== generation || controller.signal.aborted) return;
			if (cause instanceof ApiError && (cause.status === 401 || cause.status === 403)) {
				invalidateLibraryCache();
				clearAccount();
				requiresSignIn = cause.status === 401;
				error = requiresSignIn ? 'Sign in to load your library.' : 'You do not have access to these collections.';
			} else {
				error = 'Could not load your collections. Try again.';
			}
		} finally {
			if (requestGeneration === generation) {
				loading = false;
				activeRequest = null;
			}
		}
	}

	$effect(() => {
		void refresh(kind);
		return cancel;
	});

	onMount(() => {
		const onStorage = (event: StorageEvent): void => {
			if (event.key === LIBRARY_AUTH_TRANSITION_KEY) clearAccount();
		};
		window.addEventListener('storage', onStorage);
		const unsubscribe = subscribeLocalAuthTransition(clearAccount);
		return () => {
			cancel();
			window.removeEventListener('storage', onStorage);
			unsubscribe();
		};
	});
</script>

<section aria-label="Library collections" data-testid="library-collections" data-density={density}>
	<div class="nt-collection-toolbar">
		<label for="collection-find">Search in Your Library</label>
		<input id="collection-find" type="search" bind:value={query} />
		<label for="collection-sort">Sort collections</label>
		<select id="collection-sort" bind:value={sort}>
			<option value="original">Catalogue order</option>
			<option value="title">A–Z</option>
			<option value="creator">Creator</option>
		</select>
		<button type="button" class="nt-button nt-button--ghost" aria-pressed={descending}
			onclick={() => { descending = !descending; }}>Reverse order</button>
		<label for="collection-view">Collection view</label>
		<select id="collection-view" bind:value={view}>
			<option value="list">List</option>
			<option value="grid">Grid</option>
		</select>
	</div>

	<p class="nt-collection-note">Search and sort apply to the loaded first page (up to 50 albums and 50 artists). Catalogue order is not listening recency.</p>
	{#if kind === 'all'}
		<a class="nt-liked-entry" href="/library/liked">Liked Songs</a>
	{/if}
	{#if loading}
		<p role="status">Loading your collections…</p>
	{:else if error}
		<p role="alert">{error}</p>
		{#if requiresSignIn}<a class="nt-button" href="/login">Sign in</a>{/if}
		<button type="button" class="nt-button" onclick={() => void refresh(kind)}>Retry collections</button>
	{:else}
		<p role="status">Showing {displayed.length} of {items.length} loaded collections.</p>
		{#if items.length === 0}
			<p>No {kind === 'all' ? 'collections' : kind} in your library yet.</p>
		{:else if displayed.length === 0}
			<p>No matching collections in this page.</p>
			<button type="button" class="nt-button" onclick={() => { query = ''; }}>Clear collection search</button>
		{:else}
			<ul class="nt-collection-items" data-view={view}>
				{#each displayed as item (`${item.kind}:${item.id}`)}
					<li class="nt-collection-item" data-testid="library-collection" data-kind={item.kind}>
						<Artwork src={item.artUrl} alt={`${item.title} artwork`} class="nt-collection-art" />
						<div class="nt-collection-description">
							<h2>{item.title}</h2>
							<p>{item.kind === 'album' ? 'Album' : 'Artist'}{item.creator ? ` · ${item.creator}` : ''}{item.year !== null ? ` · ${item.year}` : ''}</p>
						</div>
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</section>

<style>
	.nt-collection-toolbar {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--nt-space-2) var(--nt-space-3);
		margin-block: var(--nt-space-3);
	}
	input, select {
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius-sm);
		min-height: var(--nt-control);
		min-width: 0;
		max-width: 100%;
		padding: var(--nt-space-2);
	}
	.nt-collection-note, .nt-collection-description p { color: var(--nt-fg-muted); }
	.nt-liked-entry { display: inline-block; padding-block: var(--nt-space-3); }
	.nt-collection-items { list-style: none; padding: 0; }
	.nt-collection-item {
		display: flex;
		align-items: center;
		gap: var(--nt-space-px-12);
		padding: var(--nt-space-px-8);
		min-height: var(--nt-control-lg);
		box-sizing: border-box;
	}
	[data-density='compact'] .nt-collection-item { min-height: var(--nt-control); padding-block: var(--nt-space-px-4); }
	.nt-collection-description { min-width: 0; }
	h2, .nt-collection-description p { margin: 0; overflow-wrap: anywhere; }
	h2 { font-size: var(--nt-text-base); font-weight: var(--nt-weight-strong); }
	.nt-collection-item :global(.nt-collection-art) {
		width: var(--nt-space-px-40);
		height: var(--nt-space-px-40);
		flex: 0 0 var(--nt-space-px-40);
		object-fit: cover;
		border-radius: var(--nt-radius-pane);
	}
	[data-kind='artist'] :global(.nt-collection-art) { border-radius: var(--nt-radius-pill); }
	[data-view='grid'] {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(min(100%, 270px), 1fr));
		gap: var(--nt-space-4) var(--nt-space-px-32);
	}
	[data-view='grid'] .nt-collection-item { display: block; padding: var(--nt-space-2); }
	[data-view='grid'] :global(.nt-collection-art) { width: 100%; height: auto; aspect-ratio: 1; margin-bottom: var(--nt-space-2); }
</style>
