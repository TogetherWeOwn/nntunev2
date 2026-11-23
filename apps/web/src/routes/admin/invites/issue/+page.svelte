<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { beforeNavigate } from '$app/navigation';
	import { ADMIN_INVITE_BATCH_MAX, ApiClient, ApiError, INVITE_POLICY_BOUNDS, isInvitePolicyValue } from '#lib/api/client.js';
	import { observeAccountSession, type AccountSession as Fence } from '#lib/auth-fence.js';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import { LIBRARY_AUTH_TRANSITION_KEY } from '#lib/library/cache.js';

	const account = observeAccountSession(clear);
	const [min, max] = INVITE_POLICY_BOUNDS.invite_default_expiry_days;
	const dateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });
	const unavailable = 'Invite creation is unavailable. Reopen this page to check admin access.';
	const unconfirmed = 'No link was received. An invite may have been created. Check the invite ledger before trying again.';
	const unconfirmedBatch = 'No batch links were received. The invites may have been created. Check the invite ledger before trying again.';
	let owner = $state.raw<Fence>();
	let request = $state<AbortController>();
	let writeInFlight = false;
	let batch = $state(false);
	let count = $state<number | undefined>(1);
	let email = $state('');
	let expiry = $state<number | null>();
	let invite = $state.raw<{ text: string; count: number; expires: Date }>();
	let copying = $state(false);
	let error = $state('');
	let notice = $state('');
	let heading: HTMLHeadingElement;
	let field = $state<HTMLInputElement | HTMLTextAreaElement>();
	let submitButton = $state<HTMLButtonElement>();
	const lostResult = () => error === unconfirmed || error === unconfirmedBatch;

	function clear(message = lostResult() ? error : writeInFlight ? (batch ? unconfirmedBatch : unconfirmed) : unavailable) {
		if (owner && heading?.parentElement?.contains(document.activeElement)) heading.focus();
		request?.abort();
		writeInFlight = false;
		request = owner = undefined;
		if (field) field.value = '';
		invite = expiry = undefined;
		count = 1;
		batch = copying = false;
		email = notice = '';
		error = message;
	}

	onMount(() => {
		const unsubscribe = subscribeLocalAuthTransition(() => clear());
		void authorize();
		return () => { account.dispose(); unsubscribe(); clear(); };
	});

	beforeNavigate(() => clear());

	async function run<T>(send: (client: ApiClient, signal: AbortSignal) => Promise<T>, apply: (value: T, fence: Fence) => void,
		fail: (cause: unknown) => void) {
		const previous = owner;
		account.prepare();
		const attempt = new AbortController();
		request = attempt;
		try {
			const fence = previous ?? await account.capture();
			// Box the promise: hold the auth lock for dispatch and publication, not the network wait.
			const sent = fence && await fence.run(() => attempt.signal.aborted ? undefined : {
				result: send(new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, session: { owner: fence } }), attempt.signal)
					.then((value) => () => apply(value, fence), (cause) => () => fail(cause))
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
		return run((client, signal) => client.listAdminInvites(null, signal), (_, fence) => { owner = fence; },
			() => clear());
	}

	function signupLink(code: string) {
		const url = new URL('/signup', location.origin);
		url.hash = new URLSearchParams({ invite: code }).toString();
		return url.href;
	}

	async function issue(event: SubmitEvent) {
		event.preventDefault();
		if (!owner || request || invite) return;
		if (!owner.isCurrent()) return clear();
		const isBatch = batch;
		const address = isBatch ? '' : email.trim();
		const size = isBatch ? count ?? 0 : 1;
		error = notice = '';
		if (!Number.isInteger(size) || size < 1 || size > ADMIN_INVITE_BATCH_MAX) {
			error = `Enter a whole number from 1 to ${ADMIN_INVITE_BATCH_MAX}.`;
			return;
		}
		if (address && (!address.split('@')[1]?.includes('.') || new TextEncoder().encode(address).length > 254)) {
			error = 'Enter a full email address, such as name@example.test.';
			return;
		}
		const days = expiry ?? undefined;
		if (days !== undefined && !isInvitePolicyValue('invite_default_expiry_days', days)) {
			error = `Enter a whole number from ${min} to ${max}, or leave expiry blank.`;
			return;
		}
		await run(async (client, signal) => {
			writeInFlight = true;
			if (isBatch) return client.issueAdminInviteBatch({ count: size, expires_in_days: days }, signal);
			const { code, expires_at } = await client.issueAdminInvite({ email: address || undefined, expires_in_days: days }, signal);
			return { codes: [code], expires_at };
		}, ({ codes, expires_at }) => {
			invite = { text: codes.map(signupLink).join('\n'), count: codes.length, expires: new Date(expires_at * 1000) };
			email = '';
			expiry = undefined;
		}, (cause) => {
			if (cause instanceof ApiError) {
				if (cause.code === 'reauth_required') return clear('Sign out and sign in again, then reopen this page to create an invite.');
				if ([401, 403, 404].includes(cause.status) || cause.code === 'account_deleting') return clear(unavailable);
				if (cause.status === 422) {
					error = isBatch ? 'The server rejected the batch. Check the count and expiry.' :
						'The server rejected the invite. Check the email and expiry.';
					return;
				}
			}
			clear(isBatch ? unconfirmedBatch : unconfirmed);
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
			await navigator.clipboard.writeText(current.text);
			if (invite === current) await hide(batch ? 'Batch links copied.' : 'Invite link copied.');
		} catch {
			if (invite !== current) return;
			if (!owner?.isCurrent()) return clear();
			error = batch ? 'Couldn’t copy automatically. Copy the selected links, then choose Hide links.' :
				'Couldn’t copy automatically. Copy the selected link, then choose Hide link.';
			field?.focus();
			field?.select();
		} finally {
			if (invite === current) copying = false;
		}
	}
</script>

<svelte:window onpagehide={() => clear()} onpageshow={({ persisted }) => {
	if (!persisted) return;
	if (writeInFlight || lostResult()) clear();
	else void authorize();
}} onstorage={({ key }) => { if (key === LIBRARY_AUTH_TRANSITION_KEY) clear(); }} />
<svelte:head><title>NNTune — Create an invite</title></svelte:head>

<main class="nt-wrap">
	<h1 tabindex="-1" bind:this={heading}>Create an invite</h1>
	<p><a href="/admin/invites">Back to invite ledger</a></p>
	{#if invite}
		<section aria-labelledby="new-invite-heading">
			<h2 id="new-invite-heading">{batch ? `Batch: ${invite.count} ${invite.count === 1 ? 'invite' : 'invites'}` : 'Your invite link'}</h2>
			<p id="link-help">{batch
				? 'Share these single-use links privately. Once hidden or after you leave, they can’t be shown again.'
				: 'Share this single-use link privately. Once hidden or after you leave, it can’t be shown again.'}</p>
			<p>{batch ? 'Shared expiry:' : 'Expires'} <time datetime={invite.expires.toISOString()}>{dateFormat.format(invite.expires)}</time>.</p>
			<label for="invite-link">{batch ? 'Signup links (one per line)' : 'Signup link'}</label>
			{#if batch}
				<textarea id="invite-link" bind:this={field} value={invite.text} rows="5" readonly autocomplete="off" aria-describedby="link-help"></textarea>
			{:else}
				<input id="invite-link" bind:this={field} value={invite.text} readonly autocomplete="off" aria-describedby="link-help" />
			{/if}
			<div class="actions">
				<button class="nt-button" type="button" onclick={copy} disabled={copying}>{batch ? 'Copy all links' : 'Copy link'}</button>
				<button class="nt-button nt-button--ghost" type="button" onclick={() => hide(batch ? 'Batch links hidden.' : 'Invite link hidden.')}>
					{batch ? 'Hide links' : 'Hide link'}
				</button>
			</div>
		</section>
	{:else if owner}
		<form onsubmit={issue} autocomplete="off">
			<label for="invite-mode">Issuance mode</label>
			<select id="invite-mode" bind:value={batch} disabled={!!request}>
				<option value={false}>Single invite</option>
				<option value={true}>Invite batch</option>
			</select>
			{#if batch}
				<label for="invite-count">Number of invites</label>
				<input id="invite-count" type="number" bind:value={count} required min="1" max={ADMIN_INVITE_BATCH_MAX} step="1"
					aria-describedby="count-help" disabled={!!request} />
				<small id="count-help">Enter 1–{ADMIN_INVITE_BATCH_MAX}. Batch links are not restricted to an email address.</small>
			{:else}
				<label for="invite-email">Email (optional)</label>
				<input id="invite-email" type="email" bind:value={email} maxlength="254" aria-describedby="email-help" disabled={!!request} />
				<small id="email-help">If set, only this email address can redeem the invite.</small>
			{/if}
			<label for="invite-expiry">Expiry in days (optional)</label>
			<input id="invite-expiry" type="number" bind:value={expiry} {min} {max} step="1" aria-describedby="expiry-help" disabled={!!request} />
			<small id="expiry-help">Enter {min}–{max} days, or leave blank to use the configured default.</small>
			<button class="nt-button" bind:this={submitButton} disabled={!!request}>
				{batch ? (request ? 'Creating batch…' : 'Create batch') : (request ? 'Creating invite…' : 'Create invite')}
			</button>
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
	input, select, textarea, button { min-height: 44px; font: inherit; }
	input, select, textarea { min-width: 0; padding-inline: var(--nt-space-2); background: var(--nt-bg); color: var(--nt-fg); border: 1px solid var(--nt-border); border-radius: var(--nt-radius); }
	#invite-link { display: block; width: 100%; margin-block: var(--nt-space-2); }
	textarea { resize: vertical; overflow-wrap: anywhere; }
	button { justify-self: start; }
	.actions { display: flex; flex-wrap: wrap; gap: var(--nt-space-2); }
</style>
