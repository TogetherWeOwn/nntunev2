<script lang="ts">
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient, ApiError } from '../api/client.js';
	import { subscribeLocalAuthTransition } from '../auth-transition.js';
	import { browserStorage, LIBRARY_AUTH_TRANSITION_KEY } from './cache.js';
	import { favoritesKey } from './favorites.js';
	import { importDeviceLikes, loadLikes, type LibraryLike } from './likes.js';

	let { titles, deviceCount, readDeviceIds, retireDeviceIds }: {
		titles: ReadonlyMap<string, string>;
		deviceCount: number;
		readDeviceIds: () => string[];
		retireDeviceIds: (ids: readonly string[]) => void;
	} = $props();

	type Phase = 'loading' | 'ready' | 'sign-in' | 'error';
	let phase = $state<Phase>('loading');
	let likes = $state<LibraryLike[]>([]);
	let importing = $state(false);
	let feedback = $state('');
	let active: AbortController | undefined;
	const songs = $derived(likes.filter((like) => like.kind === 'track'));

	function clientFor(request: AbortController): ApiClient {
		return new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, signal: request.signal });
	}

	async function read(client: ApiClient, isCurrent: () => boolean): Promise<void> {
		try {
			const items = await loadLikes(client);
			if (!isCurrent()) return;
			likes = items;
			phase = 'ready';
		} catch (error) {
			if (!isCurrent()) return;
			likes = [];
			phase = error instanceof ApiError && error.status === 401 ? 'sign-in' : 'error';
		}
	}

	async function load(): Promise<void> {
		active?.abort();
		const request = new AbortController();
		active = request;
		phase = 'loading';
		await read(clientFor(request), () => !request.signal.aborted);
	}

	async function importLikes(): Promise<void> {
		if (phase !== 'ready' || importing || !deviceCount) return;
		const session = browserStorage('session');
		const key = favoritesKey(session);
		if (!key) return;
		const ids = readDeviceIds();
		if (!ids.length) return;
		active?.abort();
		const request = new AbortController();
		active = request;
		const isCurrent = () => !request.signal.aborted && favoritesKey(session) === key;
		const client = clientFor(request);
		importing = true;
		feedback = '';
		try {
			await importDeviceLikes(client, ids, { isCurrent, retireConfirmed: retireDeviceIds });
			if (isCurrent()) {
				feedback = readDeviceIds().length ? 'Some favorites remain on this device.' : 'Device favorites imported.';
			}
		} catch {
			if (isCurrent()) {
				feedback = 'Import could not finish. Unconfirmed favorites are still on this device.';
			}
		} finally {
			if (isCurrent()) await read(client, isCurrent);
			if (active === request) importing = false;
		}
	}

	$effect(() => {
		function reset(): void {
			active?.abort();
			likes = [];
			feedback = '';
			importing = false;
			phase = 'sign-in';
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

<section aria-labelledby="liked-songs-heading">
	<h2 id="liked-songs-heading">Liked Songs</h2>
	{#if phase === 'loading'}
		<p role="status">Loading your liked songs…</p>
	{:else if phase === 'sign-in'}
		<p><a href="/login">Sign in</a> to see your liked songs.</p>
	{:else if phase === 'error'}
		<p role="alert">Could not load your liked songs.</p>
		<button class="nt-button nt-button--ghost" type="button" onclick={load}>Retry liked songs</button>
	{:else}
		{#if songs.length === 0}
			<p>No songs saved to this account yet.</p>
		{:else}
			<ul>
				{#each songs as song (song.targetId)}
					<li>{titles.get(song.targetId) ?? 'Unknown track'}</li>
				{/each}
			</ul>
		{/if}
		{#if deviceCount > 0 || importing || feedback}
			{#if deviceCount > 0}<p>Import favorites from this device into your signed-in account.</p>{/if}
			<button class="nt-button" type="button" onclick={importLikes} aria-disabled={importing || deviceCount === 0}>
				{importing ? 'Importing…' : 'Import device favorites'}
			</button>
		{/if}
	{/if}
	<p role="status">{feedback}</p>
</section>
