<script lang="ts">
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient, ApiError } from '../api/client.js';
	import { observeAccountSession } from '../auth-fence.js';
	import { subscribeLocalAuthTransition } from '../auth-transition.js';
	import { LIBRARY_AUTH_TRANSITION_KEY } from './cache.js';
	import { loadRecentPlays, recentPlayTitles, type RecentPlay } from './recent-plays.js';

	type View =
		| { kind: 'loading' | 'sign-in' | 'error' | 'unsupported' }
		| { kind: 'ready'; plays: RecentPlay[]; titles: Map<string, string> };

	let view = $state<View>({ kind: 'loading' });
	let active: AbortController | undefined;
	const account = observeAccountSession(() => {
		active?.abort();
		view = { kind: 'error' };
	});
	const dateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });

	async function load(): Promise<void> {
		account.prepare();
		active?.abort();
		const request = new AbortController();
		active = request;
		view = { kind: 'loading' };
		const { signal } = request;
		const owner = await account.capture();
		if (signal.aborted) return;
		if (!owner?.isCurrent()) {
			view = { kind: navigator.locks ? 'error' : 'unsupported' };
			return;
		}
		const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, signal, session: { owner } });
		const publish = (next: View) => owner.run(() => {
			if (signal.aborted) return;
			view = next;
			return true;
		});
		try {
			const sent = await owner.run(() => signal.aborted ? undefined : {
				plays: loadRecentPlays(client)
			});
			if (!sent) throw new Error('Could not verify listening history session');
			const plays = await sent.plays;
			let titles = new Map<string, string>();
			if (plays.length > 0) {
				try {
					const listed = await owner.run(() => signal.aborted ? undefined : {
						items: client.get<unknown>('/v1/library')
					});
					if (listed) titles = recentPlayTitles(await listed.items);
				} catch (error) {
					if (error instanceof ApiError && error.status === 401) throw error;
					// A missing catalogue label is not a missing play.
				}
			}
			if (!await publish({ kind: 'ready', plays, titles }) && !signal.aborted) view = { kind: 'error' };
		} catch (error) {
			const signedOut = error instanceof ApiError && error.status === 401;
			if (!await publish({ kind: signedOut ? 'sign-in' : 'error' }) && !signal.aborted) view = { kind: 'error' };
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
			account.dispose();
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
	{:else if view.kind === 'unsupported'}
		<p role="status">Listening history requires a browser with Web Locks support.</p>
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
