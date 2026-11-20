<script lang="ts">
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient, ApiError } from '../api/client.js';
	import { subscribeLocalAuthTransition } from '../auth-transition.js';
	import { ARTWORK_PLACEHOLDER_SRC } from '../player/artwork.js';
	import { LIBRARY_AUTH_TRANSITION_KEY } from './cache.js';
	import { COLLECTION_LIMIT, displayCollections, loadCollections, type Collection, type CollectionFilter, type CollectionSort } from './collections.js';

	let { filter }: { filter: CollectionFilter } = $props();
	let query = $state('');
	let sort = $state<CollectionSort>('catalogue');
	let reverse = $state(false);
	let layout = $state<'list' | 'grid'>('list');
	let status = $state<'loading' | 'ready' | 'sign-in' | 'error'>('loading');
	let rows = $state<Collection[]>([]);
	let active: AbortController | undefined;
	const visible = $derived(displayCollections(rows, query, sort, reverse));

	async function load(selected: CollectionFilter): Promise<void> {
		active?.abort();
		const request = new AbortController();
		active = request;
		rows = [];
		status = 'loading';
		try {
			const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, signal: request.signal });
			const result = await loadCollections(client, selected);
			if (!request.signal.aborted) {
				rows = result;
				status = 'ready';
			}
		} catch (error) {
			if (!request.signal.aborted) {
				status = error instanceof ApiError && error.status === 401 ? 'sign-in' : 'error';
				request.abort();
			}
		}
	}

	$effect(() => {
		void load(filter);
		return () => active?.abort();
	});

	$effect(() => {
		function reset(): void {
			active?.abort();
			rows = [];
			query = '';
			status = 'sign-in';
		}
		function onStorage(event: StorageEvent): void {
			if (event.key === LIBRARY_AUTH_TRANSITION_KEY) reset();
		}
		const unsubscribe = subscribeLocalAuthTransition(reset);
		window.addEventListener('storage', onStorage);
		return () => {
			unsubscribe();
			window.removeEventListener('storage', onStorage);
		};
	});
</script>

<section aria-label="Library collections">
	<div class="nt-collection-toolbar">
		<label>Search in Your Library <input type="search" bind:value={query} /></label>
		<label>Sort collections
			<select bind:value={sort}>
				<option value="catalogue">Catalogue order</option>
				<option value="title">A–Z</option>
				<option value="creator">Creator</option>
			</select>
		</label>
		<button class="nt-button nt-button--ghost" type="button" aria-pressed={reverse}
			onclick={() => { reverse = !reverse; }}>Reverse order</button>
		<label>Collection view
			<select bind:value={layout}>
				<option value="list">List</option>
				<option value="grid">Grid</option>
			</select>
		</label>
	</div>

	{#if status === 'loading'}
		<p role="status">Loading your collections…</p>
	{:else if status === 'sign-in'}
		<p><a href="/login">Sign in</a> to see your collections.</p>
	{:else if status === 'error'}
		<p role="alert">Could not load your collections.</p>
		<button class="nt-button" type="button" onclick={() => void load(filter)}>Retry collections</button>
	{:else if rows.length === 0}
		<p role="status">No {filter === 'all' ? 'collections' : filter} in your library yet.</p>
	{:else}
		<p role="status">Showing {visible.length} of {rows.length} loaded items.</p>
		<p>Search and sort cover the first {COLLECTION_LIMIT} items of each collection type.</p>
		{#if visible.length === 0}
			<button class="nt-button" type="button" onclick={() => { query = ''; }}>Clear collection search</button>
		{:else}
			<ul role="list" data-view={layout}>
				{#each visible as row (`${row.kind}:${row.id}`)}
					<li data-kind={row.kind}>
						<img alt="" src={row.artPath ?? ARTWORK_PLACEHOLDER_SRC} loading="lazy" referrerpolicy="no-referrer"
							onerror={({ currentTarget }) => {
								if (currentTarget.getAttribute('src') !== ARTWORK_PLACEHOLDER_SRC) {
									currentTarget.setAttribute('src', ARTWORK_PLACEHOLDER_SRC);
								}
							}} />
						<div>
							<strong>{row.title}</strong>
							<p>{row.kind}{row.creator ? ` · ${row.creator}` : ''}</p>
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
		flex-wrap: wrap;
		align-items: center;
		gap: var(--nt-space-2) var(--nt-space-3);
		margin-block: var(--nt-space-3);
	}
	label { display: grid; gap: var(--nt-space-1); max-width: 100%; }
	input, select {
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius-sm);
		min-height: var(--nt-control);
		min-width: 0;
		padding-inline: var(--nt-space-2);
	}
	ul { list-style: none; padding: 0; }
	li {
		display: flex;
		align-items: center;
		gap: var(--nt-space-3);
		padding: var(--nt-library-row-padding) var(--nt-space-2);
		min-height: calc(var(--nt-control) + 2 * var(--nt-library-row-padding));
		box-sizing: border-box;
		overflow-wrap: anywhere;
	}
	li p { margin: 0; color: var(--nt-fg-muted); }
	img {
		width: var(--nt-space-px-40);
		aspect-ratio: 1;
		flex-shrink: 0;
		object-fit: cover;
		border-radius: var(--nt-radius-pane);
	}
	[data-kind='Artist'] img { border-radius: var(--nt-radius-pill); }
	[data-view='grid'] {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(min(100%, 10rem), 1fr));
		gap: var(--nt-space-3);
	}
	[data-view='grid'] li { display: block; min-width: 0; }
	[data-view='grid'] img { display: block; width: 100%; margin-bottom: var(--nt-space-2); }
</style>
