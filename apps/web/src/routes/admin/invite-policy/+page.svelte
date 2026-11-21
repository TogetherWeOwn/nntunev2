<script lang="ts">
	import { onMount } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient, ApiError, INVITE_POLICY_BOUNDS, isInvitePolicyValue, type InvitePolicyKey } from '#lib/api/client.js';
	import { captureAuthFence, subscribeAuthFenceInvalidation } from '#lib/auth-fence.js';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import { LIBRARY_AUTH_TRANSITION_KEY } from '#lib/library/cache.js';

	type Fence = NonNullable<Awaited<ReturnType<typeof captureAuthFence>>>;
	const fields = [
		{ key: 'invite_member_min_paid_months', label: 'Required paid months', unit: 'months', help: 'Cumulative paid months required to issue member invites.' },
		{ key: 'invite_member_per_window', label: 'Per-window quota', unit: 'invites', help: 'Invites each member can issue per window. Zero turns off member issuance.' },
		{ key: 'invite_default_expiry_days', label: 'Default invite expiry', unit: 'days', help: 'Member invites stop working when their window ends, even if they have time left.' }
	] as const;
	type Row = (typeof fields)[number] & { confirmed: number; draft: string; error: string; saved: boolean };
	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });
	const unavailable = 'Invite policy is unavailable. Reload this page to check access again.';
	let owner: Fence | undefined;
	let rows = $state<Row[]>([]);
	let request = $state<AbortController>();
	let saving = $state<InvitePolicyKey>();
	let error = $state('');
	let heading: HTMLHeadingElement;

	function clear(message = unavailable) {
		if (rows.length && heading?.parentElement?.contains(document.activeElement)) heading.focus();
		request?.abort();
		request = saving = undefined;
		owner = undefined;
		rows = [];
		error = message;
	}

	onMount(() => {
		const lifetime = new AbortController();
		subscribeAuthFenceInvalidation(() => clear(), lifetime.signal);
		const unsubscribe = subscribeLocalAuthTransition(() => clear());
		void run(undefined, (signal) => client.readInvitePolicy(signal), (policy, fence) => {
			owner = fence;
			rows = fields.map((field) => ({ ...field, confirmed: policy[field.key], draft: String(policy[field.key]), error: '', saved: false }));
		}, () => clear());
		return () => { lifetime.abort(); unsubscribe(); clear(); };
	});

	async function run<T>(fence: Fence | undefined, send: (signal: AbortSignal) => Promise<T>,
		apply: (result: T, fence: Fence) => void, fail: (cause: unknown) => void) {
		const attempt = new AbortController();
		request = attempt;
		try {
			const guard = fence ?? await captureAuthFence();
			// Box the settled outcome: hold the auth lock for dispatch, not the network wait.
			const sent = guard && await guard.run(() => attempt.signal.aborted ? undefined : {
				result: send(attempt.signal).then((value) => () => apply(value, guard), (cause) => () => fail(cause))
			});
			const publish = sent && await sent.result;
			if (attempt.signal.aborted) return;
			const applied = publish && await guard?.run(() => {
				if (!attempt.signal.aborted) publish();
				return true;
			});
			if (!applied && !attempt.signal.aborted) clear();
		} finally {
			if (request === attempt) request = saving = undefined;
		}
	}

	const parseDraft = (draft: string) => /^[0-9]+$/.test(draft) ? Number(draft) : NaN;

	function save(event: SubmitEvent, row: Row) {
		event.preventDefault();
		if (request || !owner) return;
		const { key, draft } = row;
		const value = parseDraft(draft);
		if (!isInvitePolicyValue(key, value)) {
			const [min, max] = INVITE_POLICY_BOUNDS[key];
			row.error = `Enter a whole number from ${min} to ${max}.`;
			return;
		}
		if (value === row.confirmed) return;
		row.error = '';
		row.saved = false;
		saving = key;
		void run(owner, (signal) => client.writeInvitePolicy(key, value, signal), (confirmed) => {
			row.confirmed = confirmed;
			row.draft = String(confirmed);
			row.saved = true;
		}, (cause) => {
			if (cause instanceof ApiError) {
				if (cause.code === 'reauth_required') return clear('Sign out and sign in again, then reopen this page to make changes.');
				if ([401, 403, 404].includes(cause.status)) return clear();
				if (cause.status === 422) {
					row.error = 'The server rejected this value. Check the allowed range before saving again.';
					return;
				}
			}
			clear('The server didn’t confirm the change. Reload and check the current value before trying again.');
		});
	}
</script>

<svelte:window onpagehide={() => clear()} onstorage={({ key }) => { if (key === LIBRARY_AUTH_TRANSITION_KEY) clear(); }} />
<svelte:head><title>NNTune — Invite policy</title></svelte:head>

<main class="nt-wrap">
	<h1 tabindex="-1" bind:this={heading}>Invite policy</h1>
	{#if error}<p role="alert">{error}</p>{/if}
	{#if rows.length}
		<p><a href="/admin/invite-windows">Invite windows</a></p>
		<p>Save each setting separately.</p>
		{#each rows as row (row.key)}
			{@const [min, max] = INVITE_POLICY_BOUNDS[row.key]}
			<form onsubmit={(event) => save(event, row)}>
				<label for={row.key}>{row.label} ({row.unit})</label>
				<div id={`${row.key}-help`}>{row.help} Range: {min}–{max}.</div>
				<div class="controls">
					<input id={row.key} inputmode="numeric" bind:value={row.draft} readonly={saving === row.key}
						aria-describedby={`${row.key}-help${row.error ? ` ${row.key}-error` : ''}`}
						aria-invalid={!!row.error} oninput={() => { row.error = ''; row.saved = false; }} />
					<button class="nt-button" aria-label={`Save ${row.label.toLowerCase()}`}
						aria-disabled={!!request || parseDraft(row.draft) === row.confirmed}>Save</button>
				</div>
				{#if row.error}<div id={`${row.key}-error`} role="alert">{row.error}</div>{/if}
				<div role="status">{saving === row.key ? 'Saving…' : row.saved ? `Saved ${row.confirmed} ${row.unit}.` : ''}</div>
			</form>
		{/each}
	{:else if !error}
		<p role="status">Checking access…</p>
	{/if}
</main>

<style>
	form { max-width: 32rem; border-top: 1px solid var(--nt-border); padding-block: var(--nt-space-3); }
	label { display: block; font-weight: 600; }
	.controls { display: flex; flex-wrap: wrap; gap: var(--nt-space-2); margin-block: var(--nt-space-2); }
	input, button { min-height: 44px; font: inherit; }
	input {
		width: 8rem;
		max-width: 100%;
		padding-inline: var(--nt-space-2);
		background: var(--nt-bg);
		color: var(--nt-fg);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}
	button { font-weight: 600; }
	button[aria-disabled='true'] { opacity: 0.65; }
</style>
