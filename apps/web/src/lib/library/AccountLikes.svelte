<script lang="ts">
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { onDestroy, untrack } from 'svelte';
	import { ApiClient, ApiError } from '../api/client.js';
	import { observeAccountSession, type AccountSession } from '../auth-fence.js';
	import { verifiedFavoritesKey, type SnapshotStorage } from './cache.js';
	import { importDeviceLikes, loadLikes, type LibraryLike } from './likes.js';
	import { verifyLibrarySession } from './session-verification.js';

	let { titles, libraryBinding, deviceScope, storageScope, deviceCount, readDeviceIds, retireDeviceIds, onSessionChanged, onVerificationSuspended }: {
		titles: ReadonlyMap<string, string>;
		libraryBinding?: string;
		deviceScope?: string;
		storageScope: SnapshotStorage | null;
		deviceCount: number;
		readDeviceIds: () => string[];
		retireDeviceIds: (key: string, ids: readonly string[], verify: () => Promise<boolean>, run: AccountSession['run']) => Promise<boolean>;
		onSessionChanged: () => void;
		onVerificationSuspended: () => void;
	} = $props();
	let phase = $state<'loading' | 'ready' | 'sign-in' | 'error'>('loading');
	let likes = $state<LibraryLike[]>([]);
	let shelfBinding = $state<string>();
	let shelfSession: AccountSession | undefined;
	let importing = $state(false), feedback = $state('');
	let active: AbortController | undefined;
	let requestedBinding = $state<string>();
	const account = observeAccountSession(() => clearShelf('error'));
	const songs = $derived(likes.filter((like) => like.kind === 'track'));
	const canImport = $derived(!!shelfBinding && shelfBinding === libraryBinding && !!deviceScope && deviceCount > 0);

	function begin() {
		active?.abort();
		active = new AbortController();
		return active.signal;
	}

	function clientFor(owner: AccountSession, signal: AbortSignal) {
		return new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, signal, session: { owner } });
	}

	function sessionChanged() {
		clearShelf('sign-in');
		onSessionChanged();
	}

	function clearShelf(next: 'loading' | 'error' | 'sign-in') {
		active?.abort();
		likes = [];
		shelfSession = shelfBinding = undefined;
		feedback = '';
		importing = false;
		phase = next;
	}

	async function read(signal: AbortSignal, owner: AccountSession, expected?: string) {
		const run: AccountSession['run'] = (work) => owner.run(() =>
			!signal.aborted && libraryBinding === expected ? work() : undefined);
		try {
			const started = await run(() => ({ response: loadLikes(clientFor(owner, signal)) }));
			if (!started) throw new Error('No account receipt');
			const result = await started.response;
			const matches = await verify(run, result.binding, signal, owner);
			if (matches === undefined || !await run(() => {
				if (!matches || (expected && expected !== result.binding)) return sessionChanged();
				shelfSession = owner;
				shelfBinding = result.binding;
				likes = result.items;
				phase = 'ready';
				return true;
			})) throw new Error('No verified account likes');
		} catch (error) {
			if (signal.aborted || libraryBinding !== expected) return;
			clearShelf(error instanceof ApiError && error.status === 401 ? 'sign-in' : 'error');
			if (!owner.isCurrent()) onVerificationSuspended();
		}
	}

	// Box the request so the lock covers dispatch, never the network wait.
	async function verify(run: AccountSession['run'], binding: string, signal: AbortSignal, owner: AccountSession) {
		return (await run(() => ({ response: verifyLibrarySession(binding, signal, owner) })))?.response;
	}

	async function load(expected = libraryBinding) {
		account.prepare();
		clearShelf('loading');
		requestedBinding = expected;
		const signal = begin();
		const owner = await account.capture();
		if (signal.aborted) return;
		if (!owner) { phase = 'error'; return; }
		await read(signal, owner, expected);
	}

	async function importLikes() {
		const binding = shelfBinding, owner = shelfSession, key = deviceScope;
		if (phase !== 'ready' || importing || !canImport || !binding || !owner || !key) return;
		const signal = begin();
		const session = storageScope;
		const isCurrent = () => !signal.aborted && owner.isCurrent() && libraryBinding === binding &&
			deviceScope === key && verifiedFavoritesKey(session, binding) === key;
		const run: AccountSession['run'] = (work) => owner.run(() => isCurrent() ? work() : undefined);
		importing = true;
		feedback = '';
		try {
			const ids = await run(readDeviceIds);
			if (!ids?.length) return;
			const outcome = await importDeviceLikes(clientFor(owner, signal), ids, {
				binding, isCurrent, run,
				retireConfirmed: (confirmed) => retireDeviceIds(key, confirmed, async () => {
					const matches = await verify(run, binding, signal, owner);
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
			if (!signal.aborted && !owner.isCurrent()) onVerificationSuspended();
			if (isCurrent()) await read(signal, owner, binding);
			if (active?.signal === signal) importing = false;
		}
	}

	$effect(() => {
		const expected = libraryBinding;
		untrack(() => { void load(expected); });
		return () => active?.abort();
	});
	onDestroy(account.dispose);
</script>

<section aria-labelledby="liked-songs-heading">
	<h2 id="liked-songs-heading">Liked Songs</h2>
	{#if phase === 'loading' || requestedBinding !== libraryBinding}
		<p role="status">Loading your liked songs…</p>
	{:else if phase === 'sign-in'}
		<p><a href="/login">Sign in</a> to see your liked songs.</p>
	{:else if phase === 'error'}
		<p role="alert">Could not load your liked songs.</p>
		<button class="nt-button nt-button--ghost" type="button" onclick={() => void load()}>Retry liked songs</button>
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
	<p role="status">{requestedBinding === libraryBinding ? feedback : ''}</p>
</section>
