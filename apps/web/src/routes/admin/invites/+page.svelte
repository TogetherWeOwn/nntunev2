<script lang="ts">
	import { onMount } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ADMIN_INVITE_LIMIT, ApiClient, ApiError, INVITE_STATUSES, isInviteIssuerId, type AdminInvite, type AdminInviteFilter } from '#lib/api/client.js';
	import { observeAccountSession, type AccountSession as Fence } from '#lib/auth-fence.js';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import { LIBRARY_AUTH_TRANSITION_KEY } from '#lib/library/cache.js';

	const account = observeAccountSession(clear);
	const dateFormat = new Intl.DateTimeFormat(undefined, { timeZone: 'UTC', dateStyle: 'medium', timeStyle: 'medium' });
	const label = (status: AdminInvite['status']) => status[0].toUpperCase() + status.slice(1);
	const unavailable = 'Invite ledger is unavailable. Reload this page to check access and get a fresh list.';
	let owner = $state.raw<Fence>();
	let request = $state<AbortController>();
	let rows = $state.raw<AdminInvite[]>([]);
	let applied = $state.raw<AdminInviteFilter>(null);
	let mode = $state<'all' | 'issuer' | AdminInvite['status']>('all');
	let issuer = $state('');
	let writing = $state(false);
	let error = $state('');
	let inputError = $state('');
	let notice = $state('');
	let heading: HTMLHeadingElement;

	function clear(message = unavailable) {
		if (owner && heading?.parentElement?.contains(document.activeElement)) heading.focus();
		request?.abort();
		request = owner = undefined;
		rows = [];
		applied = null;
		mode = 'all';
		issuer = inputError = notice = '';
		writing = false;
		error = message;
	}

	onMount(() => {
		const unsubscribe = subscribeLocalAuthTransition(() => clear());
		load(null);
		return () => { account.dispose(); unsubscribe(); clear(); };
	});

	async function run<T>(send: (client: ApiClient, signal: AbortSignal) => Promise<T>, apply: (result: T, fence: Fence) => void,
		fail: (cause: unknown) => void) {
		request?.abort();
		const attempt = new AbortController();
		request = attempt;
		try {
			const fence = owner ?? await account.capture();
			// Guard dispatch and publication, not the intervening network wait.
			const sent = fence && await fence.run(() => attempt.signal.aborted ? undefined : {
				result: send(new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, session: { owner: fence } }), attempt.signal)
					.then((value) => () => apply(value, fence), (cause) => () => fail(cause))
			});
			const publish = sent && await sent.result;
			if (attempt.signal.aborted) return;
			const applied = publish && await fence?.run(() => {
				if (!attempt.signal.aborted) publish();
				return true;
			});
			if (!applied && !attempt.signal.aborted) clear();
		} finally {
			if (request === attempt) request = undefined;
		}
	}

	function load(filter: AdminInviteFilter, confirmedId?: number) {
		applied = filter;
		rows = [];
		void run((client, signal) => client.listAdminInvites(filter, signal), (result, fence) => {
			if (result.some(({ id, status }) => id === confirmedId && status !== 'revoked')) return clear('The refreshed list didn’t confirm revocation. Reload for a fresh list.');
			owner = fence;
			rows = result;
			if (confirmedId !== undefined) notice = `Invite #${confirmedId} revoked.`;
			writing = false;
		}, () => clear());
	}

	function filterInvites(event: SubmitEvent) {
		event.preventDefault();
		if (!owner || writing) return;
		if (mode === 'issuer' && !isInviteIssuerId(issuer.trim())) {
			inputError = 'Enter a 22-character user ID using letters, numbers, hyphens, or underscores.';
			return;
		}
		inputError = notice = '';
		load(mode === 'all' ? null : mode === 'issuer' ? { issuer: issuer.trim() } : { status: mode });
	}

	function revoke(row: AdminInvite) {
		if (!owner || request || (row.status !== 'active' && row.status !== 'expired')) return;
		if (!owner.isCurrent()) return clear();
		if (!window.confirm(`Revoke invite #${row.id}? It won’t be usable after revocation.`)) return;
		heading.focus();
		writing = true;
		notice = '';
		void run((client, signal) => client.revokeAdminInvite(row.id, signal), () => load(applied, row.id), (cause) => {
			if (cause instanceof ApiError) {
				if (cause.code === 'reauth_required') return clear('Sign out and sign in again, then reopen the ledger for a fresh list.');
				if ([401, 403, 404].includes(cause.status)) return clear(unavailable);
				if (cause.status === 422) return clear('The invite could not be revoked. It may already have been redeemed. Reload for a fresh list.');
			}
			clear('The server didn’t confirm revocation. Reload and check the list before trying again.');
		});
	}
</script>

<svelte:window onpagehide={() => clear()} onpageshow={({ persisted }) => { if (persisted) { clear(''); load(null); } }}
	onstorage={({ key }) => { if (key === LIBRARY_AUTH_TRANSITION_KEY) clear(); }} />
<svelte:head><title>NNTune — Invite ledger</title></svelte:head>

<main class="nt-wrap">
	<h1 tabindex="-1" bind:this={heading}>Invite ledger</h1>
	{#if error}<p role="alert">{error}</p>
	{:else if !owner}<p role="status">Loading invites…</p>
	{:else}
		<p><a href="/admin/invite-windows">Invite windows</a> · <a href="/admin/invites/issue">Create an invite</a> · <a href="/admin/invites/tree">Invite tree</a></p>
		<p>Up to {ADMIN_INVITE_LIMIT} matching invites, newest first. This is not a complete history.</p>
		<form onsubmit={filterInvites}>
			<label for="invite-filter">Filter by</label>
			<select id="invite-filter" bind:value={mode} onchange={() => inputError = ''} disabled={writing}>
				<option value="all">All invites</option>
				{#each INVITE_STATUSES as status}<option value={status}>{label(status)}</option>{/each}
				<option value="issuer">Issuer</option>
			</select>
			{#if mode === 'issuer'}
				<label for="invite-issuer">Exact issuer user ID</label>
				<input id="invite-issuer" bind:value={issuer} oninput={() => inputError = ''} disabled={writing}
					aria-invalid={!!inputError} aria-describedby={inputError ? 'issuer-error' : undefined} />
				{#if inputError}<p id="issuer-error" role="alert">{inputError}</p>{/if}
			{/if}
			<button class="nt-button" disabled={writing}>Apply filter</button>
		</form>
		<p class="result-filter">Results: {applied ? ('issuer' in applied ? `Issuer ${applied.issuer}` : label(applied.status)) : 'All invites'}</p>
		<div role="status">
			{#if notice}<p>{notice}</p>{/if}
			{#if writing}<p>{rows.length ? 'Revoking invite…' : 'Refreshing invites…'}</p>
			{:else if request}<p>Loading invites…</p>
			{:else if !rows.length}<p>No invites match this filter.</p>{/if}
		</div>
		{#if rows.length}
			<ul aria-label="Invite results">
				{#each rows as row (row.id)}
					{@const expiry = new Date(row.expires_at * 1000)}
					<li>
						<strong>Invite #{row.id}</strong> — {label(row.status)}
						<div>Issuer: {row.issued_by ?? 'Not recorded'}</div>
						<!-- HTML datetime omits the leading plus sign on expanded years. -->
						<div>Effective expiry (UTC): <time datetime={expiry.toISOString().replace(/^\+/, '')}>{dateFormat.format(expiry)}</time></div>
						{#if row.status === 'active' || row.status === 'expired'}
							<button class="nt-button" aria-label={`Revoke invite #${row.id}`} disabled={writing} onclick={() => revoke(row)}>Revoke</button>
						{/if}
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</main>

<style>
	form { display: grid; gap: var(--nt-space-2); max-width: 24rem; }
	label { font-weight: 600; }
	input, select, button { min-height: 44px; font: inherit; }
	input, select { min-width: 0; padding-inline: var(--nt-space-2); background: var(--nt-bg); color: var(--nt-fg); border: 1px solid var(--nt-border); border-radius: var(--nt-radius); }
	button { justify-self: start; margin-block-start: var(--nt-space-2); }
	ul { list-style: none; padding: 0; }
	li { border-top: 1px solid var(--nt-border); padding-block: var(--nt-space-3); }
	li, .result-filter { overflow-wrap: anywhere; }
</style>
