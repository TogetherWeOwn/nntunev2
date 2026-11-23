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
		recentFindsKey,
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
	let verificationFailed = $state(false);
	let retry = $state<() => void>(() => {});
	const account = observeAccountSession(clear);

	function clear() {
		session = null;
		owner = undefined;
		queries = [];
		verificationFailed = false;
	}

	onMount(() => {
		const lifetime = new AbortController();
		const unsubscribe = subscribeLocalAuthTransition(() => { lifetime.abort(); clear(); });
		window.addEventListener('storage', ({ key }) => {
			if (key === LIBRARY_AUTH_TRANSITION_KEY) { lifetime.abort(); clear(); }
		}, { signal: lifetime.signal });
		const load = async () => {
			const source = scopeLibraryStorage(browserStorage('session')), binding = libraryScopeBinding(source);
			account.prepare();
			const scope = await account.capture();
			if (!scope || lifetime.signal.aborted) return;
			if (binding) {
				const sent = await scope.run(() => lifetime.signal.aborted ? undefined : {
					matches: verifyLibrarySession(binding, lifetime.signal, scope).catch(() => false)
				});
				if (!await sent?.matches) return;
			}
			return scope.run(() => {
				if (lifetime.signal.aborted || libraryScopeBinding(source) !== binding) return;
				owner = scope;
				session = scopeLibraryStorage(source, scope);
				queries = loadRecentFinds(session, local);
				return true;
			});
		};
		retry = () => {
			verificationFailed = false;
			void load().then((verified) => {
				if (!lifetime.signal.aborted) verificationFailed = !verified;
			});
		};
		retry();
		return () => { lifetime.abort(); account.dispose(); unsubscribe(); clear(); };
	});

	let heading: HTMLElement | null = null;

	function clearAll(): void {
		const scope = owner, storage = session;
		const key = recentFindsKey(storage);
		if (!scope?.isCurrent() || !storage || !key) return;
		const previous = [...queries];
		queries = [];
		saveRecentFinds(session, local, queries);
		pushToast('Cleared recent searches.', {
			action: {
				label: 'Undo',
				run: () => {
					if (!scope.isCurrent() || recentFindsKey(storage) !== key) return;
					// Keep every post-clear search in its new recency position,
					// including queries also present in the cleared snapshot.
					const current = loadRecentFinds(storage, local);
					const restored = [...current, ...previous.filter((query) => !current.includes(query))]
						.slice(0, MAX_RECENT_FINDS);
					if (owner === scope && session === storage) queries = restored;
					saveRecentFinds(storage, local, restored);
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
		{#if verificationFailed}
			<p role="status">Could not verify your recent searches.</p>
			<button type="button" onclick={retry}>Retry recent searches</button>
		{:else if queries.length === 0}
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
