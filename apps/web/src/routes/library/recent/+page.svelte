<script lang="ts">
	import { browserStorage } from '#lib/library/cache.js';
	import {
		loadRecentFinds,
		MAX_RECENT_FINDS,
		saveRecentFinds
	} from '#lib/library/recent-finds.js';
	import { pushToast } from '#lib/shell/toasts.js';

	// Recent lists the visitor's own recent library searches for this auth
	// context, most recent first. Clearing persists immediately and offers
	// Undo; focus moves to the heading so it never drops to the body.
	const session = browserStorage('session');
	const local = browserStorage('local');
	let queries = $state<string[]>(loadRecentFinds(session, local));

	let heading: HTMLElement | null = null;

	function clearAll(): void {
		const previous = [...queries];
		queries = [];
		saveRecentFinds(session, local, queries);
		pushToast('Cleared recent searches.', {
			action: {
				label: 'Undo',
				run: () => {
					// Keep searches saved after Clear (the toast survives
					// navigation): stored entries missing from the cleared
					// snapshot move to the front instead of being lost.
					const added = loadRecentFinds(session, local).filter(
						(query) => !previous.includes(query)
					);
					queries = [...added, ...previous].slice(0, MAX_RECENT_FINDS);
					saveRecentFinds(session, local, queries);
				}
			}
		});
		heading?.focus();
	}
</script>

<svelte:head>
	<title>Recent — NNTune</title>
</svelte:head>

<main class="nt-wrap" id="main-content">
	<h1 tabindex="-1" bind:this={heading}>Recent</h1>
	<p><a href="/library">Back to Your Library</a></p>

	{#if queries.length === 0}
		<p>No recent searches yet. Searches you run in Your Library appear here.</p>
	{:else}
		<ul>
			{#each queries as query (query)}
				<li><a href={`/search?q=${encodeURIComponent(query)}`}>{query}</a></li>
			{/each}
		</ul>
		<button type="button" onclick={clearAll}>Clear recent searches</button>
	{/if}
</main>
