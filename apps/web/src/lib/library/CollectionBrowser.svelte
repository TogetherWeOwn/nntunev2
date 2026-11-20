<script lang="ts">
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient, ApiError } from '../api/client.js';
	import { ARTWORK_PLACEHOLDER_SRC } from '../player/artwork.js';
	import { displayCollections, loadCollections, type Collection, type CollectionFilter, type CollectionSort } from './collections.js';

	let { filter, accessRevoked }: { filter: CollectionFilter; accessRevoked: boolean } = $props();
	let query = $state('');
	let searchInput = $state<HTMLInputElement>();
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
			if (request.signal.aborted || accessRevoked || selected !== filter) return;
			rows = result;
			status = 'ready';
		} catch (error) {
			if (request.signal.aborted || accessRevoked || selected !== filter) return;
			status = error instanceof ApiError && error.status === 401 ? 'sign-in' : 'error';
			if (status === 'sign-in') query = '';
			request.abort();
		}
	}

	$effect(() => {
		if (accessRevoked) {
			rows = [];
			query = '';
			status = 'sign-in';
			return;
		}
		void load(filter);
		return () => active?.abort();
	});
</script>
<section aria-label="Library collections">
	<div class="nt-collection-toolbar">
		<label>Search in Your Library <input class="nt-button nt-button--ghost" type="search" bind:this={searchInput} bind:value={query} /></label>
		<label>Sort collections
			<select class="nt-button nt-button--ghost" bind:value={sort}>
				<option value="catalogue">Catalogue order</option>
				<option value="title">A–Z</option>
				<option value="creator">Creator</option>
			</select>
		</label>
		<button class="nt-button nt-button--ghost" type="button" aria-pressed={reverse}
			onclick={() => { reverse = !reverse; }}>Reverse order</button>
		<label>Collection view
			<select class="nt-button nt-button--ghost" bind:value={layout}>
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
	{:else}
		<p role="status">{rows.length ? `Showing ${visible.length} of ${rows.length} loaded items.` : `No ${filter === 'all' ? 'collections' : filter} in the loaded results.`}</p>
		{#if visible.length > 0}
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
		{:else if rows.length > 0}
			<button class="nt-button" type="button" onclick={() => { query = ''; searchInput?.focus(); }}>Clear collection search</button>
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
	input, select { font: inherit; background: var(--nt-bg-raised); min-height: var(--nt-control); min-width: 0; }
	ul { list-style: none; padding: 0; }
	li {
		display: flex;
		align-items: center;
		gap: var(--nt-space-3);
		padding: var(--nt-library-row-padding) var(--nt-space-2);
		min-height: calc(var(--nt-control) + 2 * var(--nt-library-row-padding));
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
