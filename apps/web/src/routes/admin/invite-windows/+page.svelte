<script lang="ts">
	import { onMount } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient, ApiError, INVITE_WINDOW_LIMIT, type InviteWindow, type InviteWindowSchedule } from '#lib/api/client.js';
	import { captureAuthFence, subscribeAuthFenceInvalidation } from '#lib/auth-fence.js';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import { LIBRARY_AUTH_TRANSITION_KEY } from '#lib/library/cache.js';

	type Fence = NonNullable<Awaited<ReturnType<typeof captureAuthFence>>>;
	type ListedWindow = InviteWindow & { closed?: boolean };
	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });
	const dateFormat = new Intl.DateTimeFormat(undefined, { timeZone: 'UTC', dateStyle: 'medium', timeStyle: 'medium' });
	const unavailable = 'Invite windows are unavailable. Reload this page to check access again.';
	let access = $state.raw<{ fence: Fence; windows: ListedWindow[] } | null>(null);
	let request = $state<AbortController>();
	let now = $state(NaN);
	let clockOffset = NaN;
	const serverNow = () => (clockOffset + performance.now()) / 1000;
	let heading: HTMLHeadingElement;
	let error = $state('');
	let notice = $state('');

	function clear(message = unavailable) {
		if (access && heading?.parentElement?.contains(document.activeElement)) heading.focus();
		request?.abort();
		request = undefined;
		access = null;
		notice = '';
		error = message;
	}

	onMount(() => {
		const lifetime = new AbortController();
		subscribeAuthFenceInvalidation(() => clear(), lifetime.signal);
		const unsubscribe = subscribeLocalAuthTransition(() => clear());
		const clock = setInterval(() => { now = serverNow(); }, 1000);
		void run(null, (signal) => client.listInviteWindows(signal), ({ windows, serverTime }, fence) => {
			// Use the same-origin HTTP Date and monotonic elapsed time, not the device clock.
			clockOffset = serverTime - performance.now();
			now = serverNow();
			access = { windows, fence };
		}, () => clear());
		return () => { lifetime.abort(); unsubscribe(); clearInterval(clock); clear(); };
	});

	async function run<T>(owner: Fence | null, send: (signal: AbortSignal) => Promise<T>,
		apply: (result: T, fence: Fence) => void, fail: (cause: unknown) => void) {
		if (request) return;
		const attempt = new AbortController();
		request = attempt;
		error = notice = '';
		try {
			const fence = owner ?? await captureAuthFence();
			// Use the same generation for the read, user intent, dispatch and publication.
			// Box the promise so the shared lock does not cover the network wait.
			const sent = await fence?.run(() => attempt.signal.aborted ? undefined : { result: send(attempt.signal) });
			if (!fence || !sent) {
				if (!attempt.signal.aborted) clear();
				return;
			}
			const publish = async (update: () => void) => {
				if (attempt.signal.aborted) return;
				const applied = await fence.run(() => {
					if (!attempt.signal.aborted) update();
					return true;
				});
				if (!applied && !attempt.signal.aborted) clear();
			};
			await sent.result.then((result) => publish(() => apply(result, fence)), (cause) => publish(() => fail(cause)));
		} finally {
			if (request === attempt) request = undefined;
		}
	}

	function write(input: InviteWindowSchedule | number, onSuccess?: () => void) {
		if (!access || request) return;
		const { fence, windows } = access;
		const close = typeof input === 'number';
		void run(fence, (signal) => client.writeInviteWindow(input, signal), (row) => {
			// A confirmed close stays ended even if the browser clock lags the server.
			access = { fence, windows: [{ ...row, closed: close }, ...windows.filter(({ id }) => id !== row.id)]
				.sort((a, b) => b.id - a.id).slice(0, INVITE_WINDOW_LIMIT) };
			notice = close ? 'Window is no longer open.' : 'Window scheduled.';
			onSuccess?.();
		}, (cause) => {
			if (cause instanceof ApiError) {
				if (cause.code === 'reauth_required') return clear('Sign out and sign in again, then reopen this page to make changes.');
				if (cause.status === 404 && close) return clear('This window is unavailable. Reload to check the latest list and your access.');
				if ([401, 403, 404].includes(cause.status)) return clear();
				if (!close && cause.status === 409) {
					error = 'That time overlaps another invite window. Reload to see recent changes, or choose different times.';
					return;
				}
				if (!close && cause.status === 422) {
					error = 'The schedule was rejected. Check the UTC times and shorten the note if needed.';
					return;
				}
			}
			clear('The server didn’t confirm the change. Reload and check the list before trying again.');
		});
	}

	function schedule(event: SubmitEvent & { currentTarget: HTMLFormElement }) {
		event.preventDefault();
		if (!access || request) return;
		const form = event.currentTarget;
		const fields = new FormData(form);
		const starts_at = Date.parse(`${fields.get('start')}Z`) / 1000;
		const ends_at = Date.parse(`${fields.get('end')}Z`) / 1000;
		const note = String(fields.get('note') ?? '').trim();
		notice = '';
		if (!Number.isSafeInteger(starts_at) || !Number.isSafeInteger(ends_at) || starts_at < 0 || ends_at <= starts_at) {
			error = 'Enter valid UTC times. The end must be after the start.';
			return;
		}
		write({ starts_at, ends_at, note: note || null }, () => form.reset());
	}

	function stateOf(row: ListedWindow, at: number) {
		if (row.starts_at === row.ends_at) return 'Canceled';
		if (row.closed || row.ends_at <= at) return 'Ended';
		if (!Number.isFinite(at)) return 'Status unavailable';
		return row.starts_at > at ? 'Scheduled' : 'Open';
	}

	function close(row: ListedWindow, button: HTMLButtonElement) {
		if (!access || request) return;
		if (!access.fence.isCurrent()) return clear();
		const state = stateOf(row, serverNow());
		if (state !== 'Scheduled' && state !== 'Open') return;
		const verb = state === 'Scheduled' ? 'Cancel' : 'Close';
		if (!window.confirm(`${verb} window #${row.id}? Unused member invites in this window will stop working.`)) return;
		button.closest('li')?.focus();
		write(row.id);
	}
</script>

<svelte:window onpagehide={() => clear()} onstorage={({ key }) => { if (key === LIBRARY_AUTH_TRANSITION_KEY) clear(); }} />
<svelte:head><title>NNTune — Invite windows</title></svelte:head>

<main class="nt-wrap">
	<h1 tabindex="-1" bind:this={heading}>Invite windows</h1>
	{#if !access && !error}<p role="status">Checking access…</p>{/if}
	{#if error}<p role="alert">{error}</p>{/if}
	{#if access}
		<p><a href="/admin/invite-policy">Invite policy</a></p>
		<p><a href="/admin/invites">Invite ledger</a></p>
		<section aria-labelledby="schedule-heading">
			<h2 id="schedule-heading">Schedule a window</h2>
			<p>Enter times in UTC. Member invites can only be created and redeemed while the window is open.</p>
			<form onsubmit={schedule}>
				<label>Start (UTC)<input type="datetime-local" name="start" required /></label>
				<label>End (UTC)<input type="datetime-local" name="end" required /></label>
				<label>Note (optional)<textarea name="note" maxlength="512" rows="2"></textarea></label>
				<button class="nt-button" aria-disabled={!!request}>Schedule window</button>
			</form>
		</section>
		<div role="status">{request ? 'Saving change…' : notice}</div>
		<section aria-labelledby="windows-heading">
			<h2 id="windows-heading">Recent windows</h2>
			{#if !access.windows.length}<p>No invite windows yet.</p>{:else}
				{#if access.windows.length === INVITE_WINDOW_LIMIT}<p>Showing up to {INVITE_WINDOW_LIMIT} recent windows.</p>{/if}
				<ul>
					{#each access.windows as row (row.id)}
						{@const state = stateOf(row, now)}
						<li tabindex="-1" aria-labelledby={`window-${row.id}`}>
							<h3 id={`window-${row.id}`}>Window #{row.id} · {state}</h3>
							<div>Start: <time datetime={new Date(row.starts_at * 1000).toISOString()}>{dateFormat.format(row.starts_at * 1000)} UTC</time></div>
							<div>Effective end: <time datetime={new Date(row.ends_at * 1000).toISOString()}>{dateFormat.format(row.ends_at * 1000)} UTC</time></div>
							{#if row.note}<p>{row.note}</p>{/if}
							{#if state === 'Open' || state === 'Scheduled'}
								<button class="nt-button nt-button--ghost" aria-disabled={!!request}
									aria-label={`${state === 'Open' ? 'Close' : 'Cancel'} window #${row.id}`}
									onclick={(event) => close(row, event.currentTarget)}>{state === 'Open' ? 'Close window' : 'Cancel window'}</button>
							{/if}
						</li>
					{/each}
				</ul>
			{/if}
		</section>
	{/if}
</main>

<style>
	form { display: grid; gap: var(--nt-space-2); max-width: 30rem; }
	input, textarea {
		display: block;
		box-sizing: border-box;
		width: 100%;
		min-width: 0;
		min-height: 44px;
		padding-inline: var(--nt-space-2);
		background: var(--nt-bg);
		color: var(--nt-fg);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
		font: inherit;
	}
	button { min-height: 44px; justify-self: start; margin-block: var(--nt-space-2); }
	button[aria-disabled='true'] { opacity: 0.65; cursor: wait; }
	ul { list-style: none; padding: 0; }
	li { padding-block: var(--nt-space-2); border-top: 1px solid var(--nt-border); overflow-wrap: anywhere; }
	h3 { margin-block: 0 var(--nt-space-1); }
</style>
