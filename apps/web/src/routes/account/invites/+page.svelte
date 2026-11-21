<script lang="ts">
	import { flushSync, onMount, tick } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient, ApiError, type MemberInviteHistory, type MemberInviteSummary } from '#lib/api/client.js';
	import { captureAuthFence, subscribeAuthFenceInvalidation } from '#lib/auth-fence.js';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import { LIBRARY_AUTH_TRANSITION_KEY } from '#lib/library/cache.js';

	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });
	const dateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });
	let copying = $state(false);
	let invite = $state<{ link: string; expires: Date; isCurrent: () => boolean } | null>(null);
	let notice = $state('');
	let error = $state('');
	let request = $state<AbortController>();
	let field = $state<HTMLInputElement>();
	let createButton = $state<HTMLButtonElement>();
	let heading = $state<HTMLHeadingElement>();

	type Fence = NonNullable<Awaited<ReturnType<typeof captureAuthFence>>>;
	const readMessages = {
		loading: 'Loading invite summary…',
		retry: 'Check your current session to see your invite summary.',
		failed: 'Couldn’t load your invite summary.',
		denied: 'Access to the invite summary was denied.',
		deleting: 'The invite summary is unavailable while your account is scheduled for deletion.',
		'signed-out': 'Sign in to see your invite summary.'
	};
	type ReadStatus = keyof typeof readMessages;
	let summary = $state.raw<
		| { kind: 'loading' | 'failed'; fence?: Fence }
		| { kind: Exclude<ReadStatus, 'loading' | 'failed'>; fence?: never }
		| { kind: 'ready'; data: MemberInviteSummary; fence: Fence }
	>({ kind: 'loading' });
	let reading: AbortController | undefined;
	let inviteHistory = $state.raw<MemberInviteHistory | null>(null);
	let historyError = $state<'' | 'failed' | 'denied'>('');
	let historyRead = $state<AbortController>();
	let historyButton = $state<HTMLButtonElement>();
	let historyStatus = $state<HTMLParagraphElement>();
	const statusLabels = { active: 'Active', redeemed: 'Redeemed', expired: 'Expired', revoked: 'Revoked' };

	const monthFormat = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });
	const secondsPerPolicyMonth = 30 * 86_400;
	function months(seconds: number) {
		// Truncate so a near miss never looks like the threshold.
		const hundredths = Math.floor(seconds / (secondsPerPolicyMonth / 100));
		return seconds > 0 && hundredths === 0 ? `<${monthFormat.format(0.01)}` : monthFormat.format(hundredths / 100);
	}

	function readFailure(cause: unknown): Exclude<ReadStatus, 'loading' | 'retry'> {
		if (cause instanceof ApiError) {
			// On these two reads, conflict means the account is being deleted.
			if (cause.status === 409 || cause.code === 'account_deleting') return 'deleting';
			if (cause.status === 401) return 'signed-out';
			if (cause.status === 403 || cause.status === 404) return 'denied';
		}
		return 'failed';
	}

	function clearHistory() {
		historyRead?.abort();
		historyRead = undefined;
		inviteHistory = null;
		historyError = '';
	}

	async function readHistory(fence: Fence) {
		if (summary.fence !== fence || historyRead ||
			historyError === 'denied' || inviteHistory?.next_cursor === null) return;
		const cursor = inviteHistory?.next_cursor ?? null;
		const attempt = new AbortController();
		historyRead = attempt;
		try {
			const sent = await fence.run(() => attempt.signal.aborted ? undefined : {
				result: client.readMemberInviteHistory(cursor, attempt.signal).then(
					(data) => ({ data }), (cause: unknown) => ({ status: readFailure(cause) })
				)
			});
			if (!sent) {
				if (!attempt.signal.aborted) clear();
				return;
			}
			const result = await sent.result;
			const applied = await fence.run(() => {
				if (attempt.signal.aborted) return;
				if ('data' in result) {
					const { invites, next_cursor } = result.data;
					// Keep keyboard focus in this section if the paging control goes away.
					if (next_cursor === null && document.activeElement === historyButton) historyStatus?.focus();
					inviteHistory = { invites: [...(inviteHistory?.invites ?? []), ...invites], next_cursor };
					historyError = '';
				} else if (result.status === 'signed-out' || result.status === 'deleting') {
					clear();
					summary = { kind: result.status };
				} else if (result.status === 'denied') {
					if (document.activeElement === historyButton) historyStatus?.focus();
					clearHistory();
					historyError = 'denied';
				} else {
					historyError = 'failed';
				}
				return true;
			});
			if (!applied && !attempt.signal.aborted) clear();
		} finally {
			if (historyRead === attempt) historyRead = undefined;
		}
	}

	async function readSummary(issuedFence?: Fence) {
		clearHistory();
		reading?.abort();
		const attempt = new AbortController();
		reading = attempt;
		summary = { kind: 'loading', fence: issuedFence };
		// A confirmed issuance can restart history without waiting for the quota refresh.
		if (issuedFence) void readHistory(issuedFence);
		try {
			const fence = issuedFence ?? await captureAuthFence();
			const sent = await fence?.run(() => attempt.signal.aborted ? undefined : {
				result: client.readMemberInvites(attempt.signal).then(
					(data) => ({ data }), (cause: unknown) => ({ status: readFailure(cause) })
				)
			});
			if (!fence || !sent) return;
			const result = await sent.result;
			await fence.run(() => {
				if (attempt.signal.aborted) return;
				if ('data' in result) {
					summary = { kind: 'ready', data: result.data, fence };
					if (!issuedFence) void readHistory(fence);
				} else if (result.status === 'failed') {
					summary = { kind: 'failed', fence: issuedFence };
				} else {
					if (result.status === 'signed-out' || result.status === 'deleting') clear();
					else clearHistory();
					summary = { kind: result.status };
				}
			});
		} finally {
			if (reading === attempt) {
				reading = undefined;
				if (summary.kind === 'loading') {
					clearHistory();
					summary = { kind: 'retry' };
				}
			}
		}
	}

	function clear() {
		clearHistory();
		reading?.abort();
		reading = undefined;
		summary = { kind: 'retry' };
		request?.abort();
		request = undefined;
		if (field) field.value = '';
		invite = null;
		copying = false;
		notice = error = '';
	}

	onMount(() => {
		const lifetime = new AbortController();
		subscribeAuthFenceInvalidation(clear, lifetime.signal);
		const unsubscribe = subscribeLocalAuthTransition(clear);
		void readSummary();
		return () => { lifetime.abort(); unsubscribe(); clear(); };
	});

	async function issue() {
		if (request || invite || summary.kind !== 'ready') return;
		const { fence } = summary;
		const attempt = new AbortController();
		request = attempt;
		error = notice = '';
		try {
			// Box the promise so the shared lock covers dispatch, not the network wait.
			const sent = await fence.run(() => attempt.signal.aborted ? undefined : { result: client.issueMemberInvite(attempt.signal) });
			if (!sent) {
				if (!attempt.signal.aborted) {
					clearHistory();
					summary = { kind: 'retry' };
					error = 'Couldn’t verify this browser session. No invite was requested.';
				}
				return;
			}
			const { code, expires_at } = await sent.result;
			await fence.run(() => {
				if (attempt.signal.aborted) return;
				const link = new URL('/signup', window.location.origin);
				link.hash = new URLSearchParams({ invite: code }).toString();
				invite = { link: link.href, expires: new Date(expires_at * 1000), isCurrent: fence.isCurrent };
			});
			if (attempt.signal.aborted) return;
			if (!invite) throw new Error('Invite was not published');
			void readSummary(fence);
			await tick();
			if (!attempt.signal.aborted) field?.focus();
		} catch (cause) {
			if (attempt.signal.aborted) return;
			if (!fence.isCurrent()) {
				clearHistory();
				summary = { kind: 'retry' };
				error = 'No link was received. Another attempt could use another invite from your quota.';
			} else if (cause instanceof ApiError && cause.status === 401) {
				clearHistory();
				summary = { kind: 'signed-out' };
				error = 'Sign in to create an invite.';
			} else if (cause instanceof ApiError && cause.code === 'account_deleting') {
				clearHistory();
				summary = { kind: 'deleting' };
				error = 'You can’t create invites while your account is scheduled for deletion.';
			} else if (cause instanceof ApiError && cause.status === 403) {
				error = 'You need an active membership and enough paid time to create an invite.';
			} else if (cause instanceof ApiError && cause.status === 409) {
				error = 'The invite window is closed or you’ve used all your invites for this window.';
			} else {
				error = 'No link was received. Another attempt could use another invite from your quota.';
			}
		} finally {
			if (request === attempt) request = undefined;
		}
	}

	async function hideLink(message: string) {
		invite = null;
		error = '';
		notice = message;
		await tick();
		if (notice === message) (createButton ?? heading)?.focus();
	}

	async function copy() {
		if (!invite || copying) return;
		if (!invite.isCurrent()) return clear();
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

<svelte:window
	onpagehide={() => flushSync(clear)}
	onpageshow={({ persisted }) => { if (persisted) { clear(); void readSummary(); } }}
	onstorage={({ key }) => { if (key === LIBRARY_AUTH_TRANSITION_KEY) clear(); }}
/>

<svelte:head><title>NNTune — Invites</title></svelte:head>

{#snippet timestamp(seconds: number)}
	{@const date = new Date(seconds * 1000)}
	<time datetime={date.toISOString()}>{dateFormat.format(date)}</time>
{/snippet}

<main>
	<h1 bind:this={heading} tabindex="-1">Invites</h1>
	<p>Create a single-use signup link for someone you’d like to invite to NNTune.</p>
	<p>Invites are available to eligible paid members during an open invite window.</p>
	<section aria-labelledby="invite-summary-heading">
		<h2 id="invite-summary-heading">Invite summary</h2>
		{#if summary.kind === 'ready'}
			{@const { paid_seconds, required_paid_seconds, entitled, eligible, per_window_limit, open_window, next_window } = summary.data}
			<p>Paid progress: {months(paid_seconds)} of {months(required_paid_seconds)} months. One policy month is 30 days.</p>
			<p>{!entitled ? 'Your membership is not active.' : eligible ? 'You meet the membership and paid-time requirements.' : 'You haven’t reached the paid-time requirement.'}</p>
			{#if open_window}
				<h3>Current window</h3>
				<p>Opened {@render timestamp(open_window.starts_at)} · Closes {@render timestamp(open_window.ends_at)}</p>
				<p>Used: {open_window.used} · Remaining: {open_window.remaining}</p>
			{:else}
				<p>Invites are closed.</p>
			{/if}
			{#if next_window}
				<h3>Next scheduled window</h3>
				<p>Opens {@render timestamp(next_window.starts_at)} · Closes {@render timestamp(next_window.ends_at)}</p>
			{:else}
				<p>No upcoming window is scheduled.</p>
			{/if}
			<p>Policy: up to {per_window_limit} per member per window. Unused invites don’t carry over.</p>
			<p>Availability is checked again when you create an invite.</p>
		{:else}
			<p role="status">{readMessages[summary.kind]}</p>
			{#if summary.kind === 'signed-out'}
				<a href="/login">Sign in</a>
			{:else if summary.kind !== 'loading' && summary.kind !== 'deleting'}
				<button class="nt-button nt-button--ghost" type="button" onclick={() => readSummary()}>Refresh summary</button>
			{/if}
		{/if}
	</section>
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
	{:else if summary.kind === 'ready'}
		<button class="nt-button" type="button" bind:this={createButton} onclick={issue}
			disabled={!!request}>{request ? 'Creating invite…' : 'Create invite'}</button>
	{/if}
	<div role="status">{notice}</div>
	{#if error}<p role="alert">{error}</p>{/if}
	<!-- Don't show refreshed history until the parallel summary read rules out an access denial. -->
	{#if summary.kind !== 'loading' && summary.fence}
		{@const fence = summary.fence}
		<section aria-labelledby="invite-history-heading">
			<h2 id="invite-history-heading">Issued invites</h2>
			{#if inviteHistory?.invites.length}
				<ul class="history-list" role="list">
					{#each inviteHistory.invites as row}
						<li>
							<strong>{statusLabels[row.status]}</strong><br />
							Issued {@render timestamp(row.created_at)}<br />
							Expiry {@render timestamp(row.expires_at)}
						</li>
					{/each}
				</ul>
			{/if}
			<p role="status" tabindex="-1" bind:this={historyStatus}>
				{#if historyRead}
					{inviteHistory ? 'Loading more invites…' : 'Loading issued invites…'}
				{:else if historyError === 'denied'}
					Access to issued invites was denied.
				{:else if historyError === 'failed'}
					{inviteHistory ? 'Couldn’t load more invites.' : 'Couldn’t load issued invites.'}
				{:else if inviteHistory}
					{inviteHistory.invites.length === 0 ? 'No invites issued yet.' :
						`Showing ${inviteHistory.invites.length} ${inviteHistory.invites.length === 1 ? 'invite' : 'invites'}.`}
				{/if}
			</p>
			{#if historyError === 'failed' || inviteHistory?.next_cursor != null}
				<button class="nt-button nt-button--ghost" type="button" bind:this={historyButton}
					aria-disabled={!!historyRead} onclick={() => readHistory(fence)}>
					{historyError === 'failed' ? 'Retry loading invites' : 'Load more invites'}
				</button>
			{/if}
		</section>
	{/if}
</main>

<style>
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
	.history-list {
		list-style: none;
		padding-inline-start: 0;
	}
	.history-list li {
		border-block-end: 1px solid var(--nt-border);
		padding-block: var(--nt-space-2);
	}
	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--nt-space-2);
	}
</style>
