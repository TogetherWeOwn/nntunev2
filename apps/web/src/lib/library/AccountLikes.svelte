<script lang="ts">
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { onMount } from 'svelte';
	import { ApiClient, ApiError } from '../api/client.js';
	import { browserStorage } from './cache.js';
	import { favoritesKey } from './favorites.js';
	import { importDeviceLikes, isLikesSession, loadLikes, type LibraryLike } from './likes.js';

	let { titles, libraryBinding, deviceCount, readDeviceIds, retireDeviceIds, onSessionChanged }: {
		titles: ReadonlyMap<string, string>;
		libraryBinding: string | undefined;
		deviceCount: number;
		readDeviceIds: () => string[];
		retireDeviceIds: (ids: readonly string[], verify: () => Promise<boolean>) => Promise<boolean>;
		onSessionChanged: () => void;
	} = $props();

	let phase = $state<'loading' | 'ready' | 'sign-in' | 'error'>('loading');
	let likes = $state<LibraryLike[]>([]);
	let shelfBinding = $state<string>();
	let importing = $state(false);
	let feedback = $state('');
	let active: AbortController | undefined;
	const songs = $derived(likes.filter((like) => like.kind === 'track'));
	const canImport = $derived(!!shelfBinding && shelfBinding === libraryBinding && deviceCount > 0);

	function clientFor(request: AbortController): ApiClient {
		return new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, signal: request.signal });
	}

	function sessionChanged(): void {
		active?.abort();
		onSessionChanged();
	}

	async function read(client: ApiClient, isCurrent: () => boolean, expected = libraryBinding): Promise<void> {
		try {
			const { items, binding } = await loadLikes(client);
			const matches = (!expected || binding === expected) && await isLikesSession(client, binding);
			if (!isCurrent()) return;
			if (!matches) return sessionChanged();
			shelfBinding = binding;
			likes = items;
			phase = 'ready';
		} catch (error) {
			if (!isCurrent()) return;
			shelfBinding = undefined;
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
		const binding = shelfBinding;
		if (phase !== 'ready' || importing || !canImport || !binding) return;
		const session = browserStorage('session');
		const key = favoritesKey(session);
		const ids = readDeviceIds();
		if (!key || !ids.length) return;
		active?.abort();
		const request = new AbortController();
		active = request;
		const isCurrent = () => !request.signal.aborted && libraryBinding === binding && favoritesKey(session) === key;
		const client = clientFor(request);
		importing = true;
		feedback = '';
		try {
			const cleaned = await importDeviceLikes(client, ids, {
				binding, isCurrent,
				retireConfirmed: (confirmed) => retireDeviceIds(confirmed, async () => {
					if (!isCurrent()) return false;
					const matches = await isLikesSession(client, binding);
					if (isCurrent() && !matches) sessionChanged();
					return matches && isCurrent();
				})
			});
			if (isCurrent()) feedback = !cleaned
				? 'Favorites were saved to your account, but device cleanup could not finish.'
				: readDeviceIds().length ? 'Some favorites remain on this device.' : 'Device favorites imported.';
		} catch (error) {
			if (!isCurrent()) return;
			if (error instanceof ApiError && (error.status === 401 || error.status === 412)) return sessionChanged();
			feedback = 'Import could not finish. Unconfirmed favorites are still on this device.';
		} finally {
			if (isCurrent()) await read(client, isCurrent, binding);
			if (active === request) importing = false;
		}
	}

	onMount(() => {
		void load();
		return () => active?.abort();
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
					<li>{(shelfBinding === libraryBinding && titles.get(song.targetId)) || 'Unknown track'}</li>
				{/each}
			</ul>
		{/if}
		{#if canImport || importing || feedback}
			{#if canImport}<p>Import favorites from this device into your signed-in account.</p>{/if}
			<button class="nt-button" type="button" onclick={importLikes} aria-disabled={importing || !canImport}>
				{importing ? 'Importing…' : 'Import device favorites'}
			</button>
		{/if}
	{/if}
	<p role="status">{feedback}</p>
</section>
