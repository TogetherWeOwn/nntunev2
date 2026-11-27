<script lang="ts">
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient, ApiError } from '../api/client.js';
	import { ARTWORK_PLACEHOLDER_SRC } from '../player/artwork.js';
	import { displayCollections, loadCollections, type Collection, type CollectionFilter, type CollectionSort } from './collections.js';
	import type { LibraryRefreshUpdate } from './refresh.js';

	let { filter, requiresSignIn, runIfCurrent, accountSession, reverify }: {
		filter: CollectionFilter; requiresSignIn: boolean; runIfCurrent: LibraryRefreshUpdate['runIfCurrent'];
		accountSession?: LibraryRefreshUpdate['accountSession']; reverify: () => Promise<void>;
	} = $props();
	let query = $state('');
	let searchInput = $state<HTMLInputElement>();
	let sort = $state<CollectionSort>('catalogue');
	let reverse = $state(false);
	let layout = $state<'list' | 'grid'>('list');
	let status = $state<'loading' | 'ready' | 'sign-in' | 'error' | 'unverified'>('loading');
	let rows = $state<Collection[]>([]);
	let active: AbortController | undefined;
	const visible = $derived(displayCollections(rows, query, sort, reverse));

	async function load(selected: CollectionFilter): Promise<void> {
		active?.abort();
		const request = new AbortController();
		active = request;
		rows = [];
		status = 'loading';
		const run = runIfCurrent;
		const owner = accountSession;
		const current = () => !request.signal.aborted && run === runIfCurrent && owner === accountSession &&
			(!owner || owner.isCurrent()) && !requiresSignIn && selected === filter;
		try {
			const started = await run?.(() => {
				if (!current()) return;
				const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, signal: request.signal, session: owner && { owner } });
				// Settle the request outside the receipt lock, including early failures.
				return { result: loadCollections(client, selected).then(
					(rows) => ({ rows, error: undefined }), (error: unknown) => ({ rows: undefined, error })
				) };
			});
			const result = await started?.result;
			if (result) await run?.(() => {
				if (!current()) return;
				rows = result.rows ?? [];
				status = result.rows ? 'ready' : result.error instanceof ApiError && result.error.status === 401 ? 'sign-in' : 'error';
				if (status === 'sign-in') query = '';
			});
		} catch {
			// A failed receipt cannot authorize publishing collection data.
		}
		if (active === request && !request.signal.aborted && status === 'loading') {
			query = '';
			status = 'unverified';
		}
	}
	$effect(() => {
		if (requiresSignIn) {
			rows = [];
			query = '';
			status = 'sign-in';
			return;
		}
		const owner = accountSession;
		const retire = () => {
			active?.abort();
			rows = [];
			query = '';
			status = 'unverified';
		};
		if (owner && !owner.isCurrent()) return retire();
		owner?.signal.addEventListener('abort', retire, { once: true });
		void load(filter);
		return () => { owner?.signal.removeEventListener('abort', retire); active?.abort(); };
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
	{:else if status === 'error' || status === 'unverified'}
		<p role="alert">Could not load your collections.</p>
		<button class="nt-button" type="button" onclick={() => void (status === 'unverified' ? reverify() : load(filter))}>Retry collections</button>
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
