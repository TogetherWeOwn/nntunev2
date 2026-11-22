<script lang="ts">
	import { onMount } from 'svelte';
	import { observeAccountSession, type AccountSession } from '#lib/auth-fence.js';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import RecentPlays from '#lib/library/RecentPlays.svelte';
	import { browserStorage, LIBRARY_AUTH_TRANSITION_KEY, libraryScopeBinding, scopeLibraryStorage } from '#lib/library/cache.js';
	import { verifyLibrarySession } from '#lib/library/session-verification.js';
	import {
		loadRecentFinds,
		MAX_RECENT_FINDS,
		saveRecentFinds
	} from '#lib/library/recent-finds.js';
	import { pushToast } from '#lib/shell/toasts.js';

	// Recent lists the visitor's own recent library searches for this auth
	// context, most recent first. Clearing persists immediately and offers
	// Undo; focus moves to the heading so it never drops to the body.
	const local = browserStorage('local');
	let session: ReturnType<typeof browserStorage> = null;
	let owner: AccountSession | undefined;
	let queries = $state<string[]>([]);
	const account = observeAccountSession(clear);

	function clear() {
		session = null;
		owner = undefined;
		queries = [];
	}

	onMount(() => {
		const lifetime = new AbortController();
		const source = browserStorage('session'), binding = libraryScopeBinding(source);
		const unsubscribe = subscribeLocalAuthTransition(() => { lifetime.abort(); clear(); });
		window.addEventListener('storage', ({ key }) => {
			if (key === LIBRARY_AUTH_TRANSITION_KEY) { lifetime.abort(); clear(); }
		}, { signal: lifetime.signal });
		void (async () => {
			const scope = await account.capture();
			if (!scope || lifetime.signal.aborted) return;
			if (binding) {
				const sent = await scope.run(() => lifetime.signal.aborted ? undefined : {
					matches: verifyLibrarySession(binding, lifetime.signal, scope).catch(() => false)
				});
				if (!await sent?.matches) return;
			}
			await scope.run(() => {
				if (lifetime.signal.aborted || libraryScopeBinding(source) !== binding) return;
				owner = scope;
				session = scopeLibraryStorage(source, scope);
				queries = loadRecentFinds(session, local);
			});
		})();
		return () => { lifetime.abort(); account.dispose(); unsubscribe(); clear(); };
	});

	let heading: HTMLElement | null = null;

	function clearAll(): void {
		const scope = owner, storage = session;
		if (!scope?.isCurrent() || !storage) return;
		const previous = [...queries];
		queries = [];
		saveRecentFinds(session, local, queries);
		pushToast('Cleared recent searches.', {
			action: {
				label: 'Undo',
				run: () => {
					if (owner !== scope || session !== storage || !scope.isCurrent()) return;
					// Keep every post-clear search in its new recency position,
					// including queries also present in the cleared snapshot.
					const current = loadRecentFinds(storage, local);
					queries = [...current, ...previous.filter((query) => !current.includes(query))]
						.slice(0, MAX_RECENT_FINDS);
					saveRecentFinds(storage, local, queries);
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

	<RecentPlays />

	<section aria-labelledby="recent-searches-heading">
		<h2 id="recent-searches-heading">Recent searches</h2>
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
	</section>
</main>
