<script lang="ts">
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient, ApiError } from '../api/client.js';
	import { subscribeLocalAuthTransition } from '../auth-transition.js';
	import { LIBRARY_AUTH_TRANSITION_KEY } from './cache.js';
	import { loadRecentPlays, recentPlayTitles, type RecentPlay } from './recent-plays.js';

	type View =
		| { kind: 'loading' | 'sign-in' | 'error' }
		| { kind: 'ready'; plays: RecentPlay[]; titles: Map<string, string> };

	let view = $state<View>({ kind: 'loading' });
	let active: AbortController | undefined;
	const dateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });

	async function load(): Promise<void> {
		active?.abort();
		const request = new AbortController();
		active = request;
		view = { kind: 'loading' };
		const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, signal: request.signal });
		try {
			const plays = await loadRecentPlays(client);
			let titles = new Map<string, string>();
			if (plays.length > 0) {
				try {
					titles = recentPlayTitles(await client.get<unknown>('/v1/library'));
				} catch (error) {
					if (error instanceof ApiError && error.status === 401) throw error;
					// A missing catalogue label is not a missing play.
				}
			}
			if (!request.signal.aborted) view = { kind: 'ready', plays, titles };
		} catch (error) {
			if (!request.signal.aborted) {
				view = { kind: error instanceof ApiError && error.status === 401 ? 'sign-in' : 'error' };
			}
		}
	}

	$effect(() => {
		function reset(): void {
			active?.abort();
			view = { kind: 'sign-in' };
		}
		function onStorage(event: StorageEvent): void {
			if (event.key === LIBRARY_AUTH_TRANSITION_KEY) reset();
		}
		const unsubscribe = subscribeLocalAuthTransition(reset);
		window.addEventListener('storage', onStorage);
		void load();
		return () => {
			active?.abort();
			unsubscribe();
			window.removeEventListener('storage', onStorage);
		};
	});
</script>

<section aria-labelledby="recent-plays-heading">
	<h2 id="recent-plays-heading">Recently played</h2>
	{#if view.kind === 'loading'}
		<p role="status">Loading listening history…</p>
	{:else if view.kind === 'sign-in'}
		<p><a href="/login">Sign in</a> to see your listening history.</p>
	{:else if view.kind === 'error'}
		<p role="alert">Could not load your listening history.</p>
		<button type="button" onclick={load}>Retry</button>
	{:else if view.kind === 'ready'}
		{#if view.plays.length === 0}
			<p>No recent plays yet.</p>
		{:else}
			<ul>
				{#each view.plays as play (play.key)}
					<li>
						{view.titles.get(play.trackId) ?? 'Unknown track'} —
						<time datetime={play.playedAt.toISOString()}>{dateFormat.format(play.playedAt)}</time>
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</section>
