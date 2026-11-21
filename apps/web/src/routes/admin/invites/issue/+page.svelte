<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient, ApiError, INVITE_POLICY_BOUNDS, isInvitePolicyValue } from '#lib/api/client.js';
	import { captureAuthFence, subscribeAuthFenceInvalidation } from '#lib/auth-fence.js';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import { LIBRARY_AUTH_TRANSITION_KEY } from '#lib/library/cache.js';

	type Fence = NonNullable<Awaited<ReturnType<typeof captureAuthFence>>>;
	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });
	const [min, max] = INVITE_POLICY_BOUNDS.invite_default_expiry_days;
	const dateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });
	const unavailable = 'Invite creation is unavailable. Reopen this page to check admin access.';
	const unconfirmed = 'No link was received. An invite may have been created. Check the invite ledger before trying again.';
	let owner = $state.raw<Fence>();
	let request = $state<AbortController>();
	let writeInFlight = false;
	let email = $state('');
	let expiry = $state<number | null>();
	let invite = $state.raw<{ link: string; expires: Date }>();
	let copying = $state(false);
	let error = $state('');
	let notice = $state('');
	let heading: HTMLHeadingElement;
	let field = $state<HTMLInputElement>();
	let submitButton = $state<HTMLButtonElement>();

	function clear(message = writeInFlight || error === unconfirmed ? unconfirmed : unavailable) {
		if (owner && heading?.parentElement?.contains(document.activeElement)) heading.focus();
		request?.abort();
		writeInFlight = false;
		request = owner = undefined;
		if (field) field.value = '';
		invite = expiry = undefined;
		copying = false;
		email = notice = '';
		error = message;
	}

	onMount(() => {
		const lifetime = new AbortController();
		subscribeAuthFenceInvalidation(() => clear(), lifetime.signal);
		const unsubscribe = subscribeLocalAuthTransition(() => clear());
		void authorize();
		return () => { lifetime.abort(); unsubscribe(); clear(); };
	});

	async function run<T>(send: (signal: AbortSignal) => Promise<T>, apply: (value: T, fence: Fence) => void,
		fail: (cause: unknown) => void) {
		const attempt = new AbortController();
		request = attempt;
		try {
			const fence = owner ?? await captureAuthFence();
			// Box the promise: hold the auth lock for dispatch and publication, not the network wait.
			const sent = fence && await fence.run(() => attempt.signal.aborted ? undefined : {
				result: send(attempt.signal).then((value) => () => apply(value, fence), (cause) => () => fail(cause))
			});
			const publish = sent && await sent.result;
			const applied = publish && await fence?.run(() => {
				if (attempt.signal.aborted) return;
				publish();
				writeInFlight = false;
				return true;
			});
			if (!applied && !attempt.signal.aborted) clear();
		} finally {
			if (request === attempt) request = undefined;
		}
	}

	function authorize() {
		clear('');
		return run((signal) => client.listAdminInvites(null, signal), (_, fence) => { owner = fence; },
			() => clear());
	}

	async function issue(event: SubmitEvent) {
		event.preventDefault();
		if (!owner || request || invite) return;
		if (!owner.isCurrent()) return clear();
		const address = email.trim();
		error = notice = '';
		if (address && (!address.split('@')[1]?.includes('.') || new TextEncoder().encode(address).length > 254)) {
			error = 'Enter a full email address, such as name@example.test.';
			return;
		}
		const days = expiry ?? undefined;
		if (days !== undefined && !isInvitePolicyValue('invite_default_expiry_days', days)) {
			error = `Enter a whole number from ${min} to ${max}, or leave expiry blank.`;
			return;
		}
		const input = { ...(address ? { email: address } : {}), ...(days === undefined ? {} : { expires_in_days: days }) };
		await run((signal) => {
			writeInFlight = true;
			return client.issueAdminInvite(input, signal);
		}, ({ code, expires_at }) => {
			const url = new URL('/signup', location.origin);
			url.hash = new URLSearchParams({ invite: code }).toString();
			invite = { link: url.href, expires: new Date(expires_at * 1000) };
			email = '';
			expiry = undefined;
		}, (cause) => {
			if (cause instanceof ApiError) {
				if (cause.code === 'reauth_required') return clear('Sign out and sign in again, then reopen this page to create an invite.');
				if ([401, 403, 404].includes(cause.status)) return clear(unavailable);
				if (cause.status === 422) {
					error = 'The server rejected the invite. Check the email and expiry.';
					return;
				}
			}
			clear(unconfirmed);
		});
		await tick();
		field?.focus();
	}

	async function hide(message: string) {
		if (!owner?.isCurrent()) return clear();
		if (field) field.value = '';
		invite = undefined;
		copying = false;
		error = '';
		notice = message;
		await tick();
		if (notice === message) submitButton?.focus();
	}

	async function copy() {
		const current = invite;
		if (!current || copying) return;
		if (!owner?.isCurrent()) return clear();
		copying = true;
		error = '';
		try {
			await navigator.clipboard.writeText(current.link);
			if (invite === current) await hide('Invite link copied.');
		} catch {
			if (invite !== current) return;
			if (!owner?.isCurrent()) return clear();
			error = 'Couldn’t copy automatically. Copy the selected link, then choose Hide link.';
			field?.focus();
			field?.select();
		} finally {
			if (invite === current) copying = false;
		}
	}
</script>

<svelte:window onpagehide={() => clear()} onpageshow={({ persisted }) => { if (persisted && error !== unconfirmed) void authorize(); }}
	onstorage={({ key }) => { if (key === LIBRARY_AUTH_TRANSITION_KEY) clear(); }} />
<svelte:head><title>NNTune — Create an invite</title></svelte:head>

<main class="nt-wrap">
	<h1 tabindex="-1" bind:this={heading}>Create an invite</h1>
	<p><a href="/admin/invites">Back to invite ledger</a></p>
	{#if invite}
		<section aria-labelledby="new-invite-heading">
			<h2 id="new-invite-heading">Your invite link</h2>
			<p id="link-help">Share this single-use link privately. Once hidden or after you leave, it can’t be shown again.</p>
			<p>Expires <time datetime={invite.expires.toISOString()}>{dateFormat.format(invite.expires)}</time>.</p>
			<label for="invite-link">Signup link</label>
			<input id="invite-link" bind:this={field} value={invite.link} readonly autocomplete="off" aria-describedby="link-help" />
			<div class="actions">
				<button class="nt-button" type="button" onclick={copy} disabled={copying}>Copy link</button>
				<button class="nt-button nt-button--ghost" type="button" onclick={() => hide('Invite link hidden.')}>Hide link</button>
			</div>
		</section>
	{:else if owner}
		<form onsubmit={issue} autocomplete="off">
			<label for="invite-email">Email (optional)</label>
			<input id="invite-email" type="email" bind:value={email} maxlength="254" aria-describedby="email-help" disabled={!!request} />
			<small id="email-help">If set, only this email address can redeem the invite.</small>
			<label for="invite-expiry">Expiry in days (optional)</label>
			<input id="invite-expiry" type="number" bind:value={expiry} {min} {max} step="1" aria-describedby="expiry-help" disabled={!!request} />
			<small id="expiry-help">Enter {min}–{max} days, or leave blank to use the configured default.</small>
			<button class="nt-button" bind:this={submitButton} disabled={!!request}>{request ? 'Creating invite…' : 'Create invite'}</button>
		</form>
	{:else if !error}
		<p role="status">Checking admin access…</p>
	{/if}
	<div role="status">{notice}</div>
	{#if error}<p role="alert">{error}</p>{/if}
</main>

<style>
	form { display: grid; gap: var(--nt-space-2); max-width: 28rem; }
	label { font-weight: 600; }
	input, button { min-height: 44px; font: inherit; }
	input { min-width: 0; padding-inline: var(--nt-space-2); background: var(--nt-bg); color: var(--nt-fg); border: 1px solid var(--nt-border); border-radius: var(--nt-radius); }
	#invite-link { display: block; width: 100%; margin-block: var(--nt-space-2); }
	button { justify-self: start; }
	.actions { display: flex; flex-wrap: wrap; gap: var(--nt-space-2); }
</style>
