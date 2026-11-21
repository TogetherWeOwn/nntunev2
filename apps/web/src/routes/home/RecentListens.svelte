<script lang="ts">
	import { onMount } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { subscribeAuthFenceInvalidation } from '#lib/auth-fence.js';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import { LIBRARY_AUTH_TRANSITION_KEY } from '#lib/library/cache.js';
	import { createRecentListensReader, type RecentListensView } from './recent-listens.js';

	const dateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });
	const eventLabel = { start: 'Playback started', complete: 'Playback completed', skip: 'Playback skipped' };
	let view = $state<RecentListensView>({ kind: 'loading' });
	const reader = createRecentListensReader(PUBLIC_API_BASE_URL, (next) => (view = next));

	onMount(() => {
		const lifetime = new AbortController();
		subscribeAuthFenceInvalidation(reader.invalidate, lifetime.signal);
		const unsubscribe = subscribeLocalAuthTransition(reader.invalidate);
		void reader.load();
		return () => {
			lifetime.abort();
			unsubscribe();
			reader.cancel();
		};
	});
</script>

<svelte:window
	onpagehide={reader.invalidate}
	onfocus={reader.checkCurrent}
	onstorage={({ key }) => { if (key === LIBRARY_AUTH_TRANSITION_KEY) reader.invalidate(); }}
/>
<svelte:document onvisibilitychange={reader.checkCurrent} />

<section aria-labelledby="home-listens-heading">
	<h2 id="home-listens-heading">Recent listens</h2>
	{#if view.kind === 'loading'}
		<p role="status">Loading listening history…</p>
	{:else if view.kind === 'empty'}
		<p>No listening events have been recorded for this account.</p>
	{:else if view.kind === 'sign-in'}
		<p><a href="/login">Sign in</a> to see your listening history.</p>
	{:else if view.kind === 'refused'}
		<p role="alert">You do not have access to this listening history.</p>
	{:else if view.kind === 'error' || view.kind === 'stale'}
		<p role="status">
			{view.kind === 'stale'
				? 'Listening history is hidden until you check the current session again.'
				: 'Could not load listening history.'}
		</p>
		<button type="button" onclick={reader.load}>Retry</button>
	{:else if view.kind === 'ready'}
		<p>Up to five recorded playback events, in the order returned by your listening history.</p>
		<ul>
			{#each view.events as event (event.key)}
				<li>
					{eventLabel[event.event]} · Track reference: <bdi>{event.track}</bdi> · Recorded
					<time datetime={event.recordedAt.toISOString()}>{dateFormat.format(event.recordedAt)}</time>
				</li>
			{/each}
		</ul>
	{/if}
</section>

<style>
	li {
		overflow-wrap: anywhere;
	}
</style>
