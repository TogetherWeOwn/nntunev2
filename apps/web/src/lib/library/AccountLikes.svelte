<script lang="ts">
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { onMount } from 'svelte';
	import { ApiClient, ApiError } from '../api/client.js';
	import { captureAuthFence } from '../auth-fence.js';
	import { browserStorage, verifiedFavoritesKey } from './cache.js';
	import { importDeviceLikes, loadLikes, type LibraryLike } from './likes.js';
	import { verifyLibrarySession } from './session-verification.js';

	type Fence = NonNullable<Awaited<ReturnType<typeof captureAuthFence>>>;
	let { titles, libraryBinding, deviceScope, deviceCount, readDeviceIds, retireDeviceIds, onSessionChanged, onVerificationSuspended }: {
		titles: ReadonlyMap<string, string>;
		libraryBinding?: string;
		deviceScope?: string;
		deviceCount: number;
		readDeviceIds: () => string[];
		retireDeviceIds: (key: string, ids: readonly string[], verify: () => Promise<boolean>, run: Fence['run']) => Promise<boolean>;
		onSessionChanged: () => void;
		onVerificationSuspended: () => void;
	} = $props();
	let phase = $state<'loading' | 'ready' | 'sign-in' | 'error'>('loading');
	let likes = $state<LibraryLike[]>([]);
	let shelfBinding = $state<string>();
	let shelfFence: Fence | undefined;
	let importing = $state(false), feedback = $state('');
	let active: AbortController | undefined;
	const songs = $derived(likes.filter((like) => like.kind === 'track'));
	const canImport = $derived(!!shelfBinding && shelfBinding === libraryBinding && !!deviceScope && deviceCount > 0);

	function begin() {
		active?.abort();
		const request = active = new AbortController();
		return { signal: request.signal, client: new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, signal: request.signal }) };
	}

	function sessionChanged() {
		active?.abort();
		onSessionChanged();
	}

	async function read({ client, signal }: ReturnType<typeof begin>, fence: Fence, expected?: string) {
		const run: Fence['run'] = (work) => fence.run(() => signal.aborted ? undefined : work());
		try {
			const started = await run(() => ({ response: loadLikes(client) }));
			if (!started) return;
			const result = await started.response;
			const matches = await verify(run, result.binding, signal);
			if (matches === undefined) return;
			await run(() => {
				if (!matches || (expected && expected !== result.binding)) return sessionChanged();
				shelfFence = fence;
				shelfBinding = result.binding;
				likes = result.items;
				phase = 'ready';
			});
		} catch (error) {
			await run(() => { phase = error instanceof ApiError && error.status === 401 ? 'sign-in' : 'error'; });
		} finally {
			if (!signal.aborted && !fence.isCurrent()) onVerificationSuspended();
			else if (!signal.aborted && phase === 'loading') phase = 'error';
		}
	}

	// Box the request so the lock covers dispatch, never the network wait.
	async function verify(run: Fence['run'], binding: string, signal: AbortSignal) {
		return (await run(() => ({ response: verifyLibrarySession(binding, signal) })))?.response;
	}

	async function load() {
		const request = begin(), expected = libraryBinding;
		phase = 'loading';
		const fence = await captureAuthFence();
		if (request.signal.aborted) return;
		if (!fence) { phase = 'error'; return; }
		await read(request, fence, expected);
	}

	async function importLikes() {
		const binding = shelfBinding, fence = shelfFence, key = deviceScope;
		if (phase !== 'ready' || importing || !canImport || !binding || !fence || !key) return;
		const request = begin();
		const { signal, client } = request;
		const session = browserStorage('session');
		const isCurrent = () => !signal.aborted && fence.isCurrent() && libraryBinding === binding &&
			deviceScope === key && verifiedFavoritesKey(session, binding) === key;
		const run: Fence['run'] = (work) => fence.run(() => isCurrent() ? work() : undefined);
		importing = true;
		feedback = '';
		try {
			const ids = await run(readDeviceIds);
			if (!ids?.length) return;
			const outcome = await importDeviceLikes(client, ids, {
				binding, isCurrent, run,
				retireConfirmed: (confirmed) => retireDeviceIds(key, confirmed, async () => {
					const matches = await verify(run, binding, signal);
					if (matches === false && !signal.aborted) sessionChanged();
					return matches === true;
				}, run)
			});
			await run(() => {
				if (!outcome) return;
				feedback = {
					'cleanup-failed': 'Favorites were saved to your account, but device cleanup could not finish.',
					'none-confirmed': 'No device favorites were confirmed by your account.',
					partial: 'Some favorites remain on this device.',
					complete: readDeviceIds().length ? 'Some favorites remain on this device.' : 'Device favorites imported.'
				}[outcome];
			});
		} catch (error) {
			if (signal.aborted) return;
			if (error instanceof ApiError && (error.status === 401 || error.status === 412)) sessionChanged();
			else await run(() => { feedback = 'Import could not finish. Unconfirmed favorites are still on this device.'; });
		} finally {
			if (!signal.aborted && !fence.isCurrent()) onVerificationSuspended();
			if (isCurrent()) await read(request, fence, binding);
			if (active?.signal === signal) importing = false;
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
