<script lang="ts">
	import { flushSync, onMount, tick } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient, ApiError, isAdminInviteTreeDenial, isInviteTreeUserId, type AdminInviteTreePage } from '#lib/api/client.js';
	import { observeAccountSession } from '#lib/auth-fence.js';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import { LIBRARY_AUTH_TRANSITION_KEY } from '#lib/library/cache.js';

	const invalidate = () => reset(unavailable);
	const account = observeAccountSession(invalidate);
	const unavailable = 'Invite tree unavailable for this lookup. Check the user ID and your admin session.';
	let ready = $state(false);
	let input = $state('');
	let inputError = $state('');
	let error = $state('');
	let retryId = $state<string>();
	let node = $state.raw<AdminInviteTreePage>();
	let pending = $state.raw<AbortController>();
	let heading: HTMLHeadingElement;
	let selectedHeading = $state<HTMLHeadingElement>();
	let inviteesHeading = $state<HTMLHeadingElement>();
	let moreButton = $state<HTMLButtonElement>();
	function reset(message = '') {
		if (heading?.parentElement?.contains(document.activeElement)) heading.focus();
		pending?.abort();
		pending = node = retryId = undefined;
		input = inputError = '';
		error = message;
	}

	onMount(() => {
		const unsubscribe = subscribeLocalAuthTransition(invalidate);
		ready = true;
		return () => { account.dispose(); unsubscribe(); pending?.abort(); };
	});

	function lookup(event: SubmitEvent) {
		event.preventDefault();
		if (!isInviteTreeUserId(input)) {
			inputError = 'Enter an exact user ID: 1–128 letters, numbers, hyphens or underscores.';
			return;
		}
		void load(input);
	}

	async function load(id: string, previous?: AdminInviteTreePage) {
		account.prepare();
		if (previous && node !== previous) return;
		if (!previous) {
			heading.focus();
			reset();
		}
		const attempt = new AbortController();
		pending = attempt;
		const current = () => pending === attempt && !attempt.signal.aborted;
		try {
			const fence = await account.capture({ readOnly: true });
			// Do not hold the shared auth lock while waiting for the network.
			const sent = fence && await fence.run(() => current() ? {
				result: new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, session: { owner: fence } })
					.readAdminInviteTree(id, previous?.next_cursor ?? null, attempt.signal).then(
					(page) => ({ page, cause: null }), (cause: unknown) => ({ page: null, cause })
				)
			} : undefined);
			const result = sent && await sent.result;
			if (!current()) return;
			const published = result && await fence?.run(() => {
				if (!current()) return;
				const { page, cause } = result;
				if (page) {
					if (previous && page.next_cursor === null && document.activeElement === moreButton) inviteesHeading?.focus();
					node = { ...page, invitee_ids: [...new Set([...(previous?.invitee_ids ?? []), ...page.invitee_ids])] };
				} else if (cause instanceof ApiError && (isAdminInviteTreeDenial(cause.status) || cause.code === 'account_deleting')) {
					reset(unavailable);
				} else {
					reset(cause instanceof ApiError && cause.status === 422 && previous
						? 'This page is no longer available. Restart from the first page.'
						: 'Could not load the invite tree. Try again from the first page.');
					retryId = id;
				}
				return true;
			});
			if (!published && current()) reset(unavailable);
			if (!previous && result?.page) {
				await tick();
				if (current() && node && document.activeElement === heading) selectedHeading?.focus();
			}
		} finally {
			if (pending === attempt) pending = undefined;
		}
	}
</script>
<svelte:window onpagehide={() => flushSync(() => reset())} onpageshow={({ persisted }) => { if (persisted) reset(); }}
	onstorage={({ key }) => { if (key === LIBRARY_AUTH_TRANSITION_KEY) reset(unavailable); }} />
<svelte:head><title>NNTune — Invite tree</title></svelte:head>
<main class="nt-wrap">
	<h1 tabindex="-1" bind:this={heading}>Invite tree</h1>
	<p><a href="/admin/invites">Back to invite ledger</a></p>
	{#if ready}
		<form autocomplete="off" onsubmit={lookup}>
			<label for="tree-user-id">Exact user ID</label>
			<input id="tree-user-id" bind:value={input} oninput={() => inputError = ''}
				spellcheck="false" autocapitalize="none" aria-invalid={!!inputError}
				aria-describedby={inputError ? 'tree-input-error' : undefined} />
			{#if inputError}<p role="alert" id="tree-input-error">{inputError}</p>{/if}
			<button class="nt-button">Look up user</button>
		</form>
	{/if}
	{#if error}<p role="alert">{error}</p>{/if}
	{#if retryId}<button class="nt-button" onclick={() => { if (retryId) void load(retryId); }}>Restart lookup</button>{/if}
	<p role="status">{pending ? (node ? 'Loading more invitees…' : 'Loading user…') : node ? `${node.invitee_ids.length} direct invitees shown.` : ''}</p>
	{#if node}
		<h2 tabindex="-1" bind:this={selectedHeading}>Selected user</h2>
		<p>{node.user_id}</p>
		<h2>Inviter</h2>
		{#if node.inviter_id}{@render edge(node.inviter_id)}{:else}<p>No inviter recorded.</p>{/if}
		<h2 id="direct-invitees" tabindex="-1" bind:this={inviteesHeading}>Direct invitees</h2>
		{#if node.invitee_ids.length}
			<ul aria-labelledby="direct-invitees">
				{#each node.invitee_ids as id (id)}<li>{@render edge(id)}</li>{/each}
			</ul>
		{:else}<p>No direct invitees.</p>{/if}
		{#if node.next_cursor !== null}
			<button class="nt-button" bind:this={moreButton} aria-disabled={!!pending}
				onclick={() => { if (node?.next_cursor && !pending) void load(node.user_id, node); }}>Load more</button>
		{/if}
	{/if}
</main>

{#snippet edge(id: string)}<button class="nt-button nt-button--ghost" onclick={() => void load(id)}>{id}</button>{/snippet}

<style>
	main { overflow-wrap: anywhere; padding-block-end: var(--nt-space-4); }
	form { display: grid; gap: var(--nt-space-2); max-width: 24rem; }
	label { font-weight: 600; }
	input, button { min-height: 44px; min-width: 0; font: inherit; }
	input { padding-inline: var(--nt-space-2); background: var(--nt-bg); color: var(--nt-fg); border: 1px solid var(--nt-border); border-radius: var(--nt-radius); }
	button { justify-self: start; max-width: 100%; text-align: start; overflow-wrap: anywhere; }
	ul { list-style: none; display: grid; gap: var(--nt-space-2); padding: 0; }
	[role='status']:empty { margin: 0; }
</style>
