<script lang="ts">
	import { onDestroy, onMount, tick } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient, ApiError } from '#lib/api/client.js';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import { LIBRARY_AUTH_TRANSITION_KEY } from '#lib/library/cache.js';

	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });
	const dateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });
	let requesting = $state(false);
	let copying = $state(false);
	let invite = $state<{ link: string; expires: Date } | null>(null);
	let notice = $state('');
	let error = $state('');
	let signedOut = $state(false);
	let request: AbortController | undefined;
	let field = $state<HTMLInputElement>();
	let createButton = $state<HTMLButtonElement>();

	function clear() {
		request?.abort();
		request = undefined;
		if (field) field.value = '';
		invite = null;
		requesting = copying = signedOut = false;
		notice = error = '';
	}

	onMount(() => subscribeLocalAuthTransition(clear));
	onDestroy(clear);

	function onStorage(event: StorageEvent) {
		if (event.key === LIBRARY_AUTH_TRANSITION_KEY) clear();
	}

	async function issue() {
		if (request || invite || signedOut) return;
		const attempt = new AbortController();
		request = attempt;
		requesting = true;
		error = notice = '';
		try {
			const { code, expires_at } = await client.issueMemberInvite(attempt.signal);
			if (attempt.signal.aborted) return;
			const link = new URL('/signup', window.location.origin);
			// Fragments are not sent in the signup request or Referer header.
			link.hash = new URLSearchParams({ invite: code }).toString();
			invite = { link: link.href, expires: new Date(expires_at * 1000) };
			await tick();
			if (!attempt.signal.aborted) field?.focus();
		} catch (cause) {
			if (attempt.signal.aborted) return;
			if (cause instanceof ApiError && cause.status === 401) {
				signedOut = true;
				error = 'Sign in to create an invite.';
			} else if (cause instanceof ApiError && cause.code === 'account_deleting') {
				error = 'You can’t create invites while your account is scheduled for deletion.';
			} else if (cause instanceof ApiError && cause.status === 403) {
				error = 'You need an active membership and enough paid time to create an invite.';
			} else if (cause instanceof ApiError && cause.status === 409) {
				error = 'The invite window is closed or you’ve used all your invites for this window.';
			} else {
				error = 'No link was received. Another attempt could use another invite from your quota.';
			}
		} finally {
			if (request === attempt) {
				request = undefined;
				requesting = false;
			}
		}
	}

	async function hideLink(message: string) {
		invite = null;
		error = '';
		notice = message;
		await tick();
		if (notice === message) createButton?.focus();
	}

	async function copy() {
		if (!invite || copying) return;
		const { link } = invite;
		copying = true;
		error = '';
		try {
			await navigator.clipboard.writeText(link);
			if (invite?.link === link) await hideLink('Invite link copied.');
		} catch {
			if (invite?.link !== link) return;
			error = 'Couldn’t copy automatically. Copy the selected link, then choose Hide link.';
			field?.focus();
			field?.select();
		} finally {
			if (invite?.link === link || !invite) copying = false;
		}
	}
</script>

<svelte:window onpagehide={clear} onstorage={onStorage} />

<svelte:head><title>NNTune — Invites</title></svelte:head>

<main>
	<h1>Invites</h1>
	<p>Create a single-use signup link for someone you’d like to invite to NNTune.</p>
	{#if invite}
		<section aria-labelledby="new-invite-heading">
			<h2 id="new-invite-heading">Your invite link</h2>
			<p id="invite-link-help">
				Share this link privately. Copy it before leaving: once hidden, it can’t be shown again.
			</p>
			<p>Expires <time datetime={invite.expires.toISOString()}>{dateFormat.format(invite.expires)}</time>.</p>
			<label for="member-invite-link">Signup link</label>
			<input id="member-invite-link" bind:this={field} value={invite.link} readonly
				autocomplete="off" aria-describedby="invite-link-help" />
			<div class="actions">
				<button class="nt-button" type="button" onclick={copy} disabled={copying}>Copy link</button>
				<button class="nt-button nt-button--ghost" type="button" disabled={copying}
					onclick={() => hideLink('Invite link hidden.')}>Hide link</button>
			</div>
		</section>
	{:else}
		<p>Invites are available to eligible paid members during an open invite window.</p>
		{#if signedOut}
			<a href="/login">Sign in</a>
		{:else}
			<button class="nt-button" type="button" bind:this={createButton} onclick={issue}
				disabled={requesting}>{requesting ? 'Creating invite…' : 'Create invite'}</button>
		{/if}
	{/if}
	<div role="status">{notice}</div>
	{#if error}<p role="alert">{error}</p>{/if}
</main>

<style>
	section {
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius-pane);
		padding: var(--nt-space-3);
	}
	input {
		display: block;
		width: 100%;
		min-height: 44px;
		margin-block: var(--nt-space-2);
		padding-inline: var(--nt-space-2);
		background: var(--nt-bg);
		color: var(--nt-fg);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius-md);
		font: inherit;
	}
	button {
		min-height: 44px;
	}
	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--nt-space-2);
	}
</style>
