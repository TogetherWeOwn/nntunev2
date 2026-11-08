<script lang="ts">
	import { onDestroy, onMount, tick } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient } from '#lib/api/client.js';
	import Icon from '#lib/design/Icon.svelte';
	import { pushToast } from '#lib/shell/toasts.js';
	import { confirmWithPasskey, loadStatus, unlink } from './flow.js';

	/**
	 * Pre-PUT ListenBrainz section: token-free status and Unlink only. There
	 * is deliberately no username/token field, link form, Change account or
	 * PUT: the link route is not dispatched and the UI renders no dead
	 * controls. Status is never set optimistically.
	 */

	/** The status loading text only appears after this delay. */
	const LOADING_DELAY_MS = 300;

	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });
	// Teardown is permanent even when a transport or authenticator ignores abort.
	const lifetime = new AbortController();
	onDestroy(() => lifetime.abort());

	type Phase = 'loading' | 'error' | 'signed_out' | 'unlinked' | 'linked';
	let phase = $state<Phase>('loading');
	let username = $state('');
	let showLoading = $state(false);
	let online = $state(true);
	let busy = $state(false);
	let accountDeleting = $state(false);
	let unlinkError = $state<string | null>(null);
	let statusMessage = $state('');

	let heading = $state<HTMLElement>();
	let unlinkButton = $state<HTMLButtonElement>();
	let confirmDialog = $state<HTMLDialogElement>();
	let cancelButton = $state<HTMLButtonElement>();
	let reauthDialog = $state<HTMLDialogElement>();
	let reauthHeading = $state<HTMLElement>();

	let reauthWorking = $state(false);
	let reauthError = $state(false);
	let ceremony: AbortController | null = null;

	let loadTimer: ReturnType<typeof setTimeout> | undefined;

	async function refresh() {
		phase = 'loading';
		showLoading = false;
		clearTimeout(loadTimer);
		loadTimer = setTimeout(() => {
			showLoading = true;
		}, LOADING_DELAY_MS);
		const outcome = await loadStatus(client, lifetime.signal);
		clearTimeout(loadTimer);
		if (lifetime.signal.aborted) return;
		showLoading = false;
		if (outcome.kind === 'linked') {
			username = outcome.username;
			phase = 'linked';
		} else {
			phase = outcome.kind;
		}
	}

	function syncOnline() {
		online = navigator.onLine !== false;
	}

	onMount(() => {
		syncOnline();
		window.addEventListener('online', syncOnline);
		window.addEventListener('offline', syncOnline);
		void refresh();
		return () => {
			clearTimeout(loadTimer);
			window.removeEventListener('online', syncOnline);
			window.removeEventListener('offline', syncOnline);
		};
	});

	const canWrite = $derived(online && !busy);

	function openConfirm() {
		if (!canWrite || accountDeleting) return;
		unlinkError = null;
		confirmDialog?.showModal();
		cancelButton?.focus();
	}

	function closeConfirm() {
		if (confirmDialog?.open) confirmDialog.close();
	}

	async function focusHeading() {
		await tick();
		heading?.focus();
	}

	async function focusUnlink() {
		await tick();
		// An offline transition disables the invoker while a dialog is open.
		// Disabled buttons cannot receive focus after that dialog closes.
		if (unlinkButton?.isConnected && !unlinkButton.disabled) unlinkButton.focus();
		else heading?.focus();
	}

	/** Apply one DELETE result. `afterReauth` marks the single allowed retry. */
	async function runUnlink(afterReauth: boolean) {
		// Re-check at the moment of sending, not only when a dialog opened: the
		// browser may have gone offline while a dialog (or the passkey prompt)
		// was open, and a second submit must never send a second DELETE.
		syncOnline();
		if (
			lifetime.signal.aborted ||
			busy ||
			accountDeleting ||
			phase !== 'linked' ||
			navigator.onLine === false
		) {
			closeConfirm();
			await focusUnlink();
			return;
		}
		busy = true;
		statusMessage = 'Unlinking…';
		unlinkError = null;
		const outcome = await unlink(client, { afterReauth, signal: lifetime.signal });
		if (lifetime.signal.aborted) return;
		closeConfirm();
		busy = false;
		statusMessage = '';
		switch (outcome.kind) {
			case 'unlinked':
				username = '';
				phase = 'unlinked';
				statusMessage = 'ListenBrainz unlinked';
				pushToast('ListenBrainz unlinked');
				await focusHeading();
				break;
			case 'reauth_required':
				reauthError = false;
				reauthWorking = false;
				reauthDialog?.showModal();
				await tick();
				reauthHeading?.focus();
				break;
			case 'reauth_again':
				unlinkError = "Couldn't confirm it's you. Try again.";
				await focusUnlink();
				break;
			case 'signed_out':
				phase = 'signed_out';
				await focusHeading();
				break;
			case 'account_deleting':
				accountDeleting = true;
				await focusHeading();
				break;
			case 'failed':
				unlinkError = "Couldn't unlink ListenBrainz. Your link is unchanged — try again.";
				await focusUnlink();
				break;
		}
	}

	async function confirmUnlink() {
		if (busy) return;
		await runUnlink(false);
	}

	function cancelConfirm() {
		if (busy) return;
		closeConfirm();
		void focusUnlink();
	}

	/** Cancel/Esc: abort the ceremony, retry nothing, return focus to Unlink. */
	function cancelReauth() {
		ceremony?.abort();
		ceremony = null;
		reauthWorking = false;
		statusMessage = '';
		if (reauthDialog?.open) reauthDialog.close();
		void focusUnlink();
	}

	async function continueWithPasskey() {
		if (reauthWorking || busy || accountDeleting || phase !== 'linked') return;
		reauthWorking = true;
		statusMessage = 'Waiting for your passkey…';
		reauthError = false;
		const attempt = new AbortController();
		ceremony = attempt;
		const outcome = await confirmWithPasskey(client, attempt.signal);
		if (lifetime.signal.aborted || ceremony !== attempt) return;
		ceremony = null;
		reauthWorking = false;
		statusMessage = '';
		if (outcome === 'dismissed') {
			cancelReauth();
		} else if (outcome === 'failed') {
			reauthError = true;
		} else {
			if (reauthDialog?.open) reauthDialog.close();
			await runUnlink(true);
		}
	}

	onDestroy(() => ceremony?.abort());

	// Native showModal trapping stutters in Chromium; wrap Tab at the
	// boundaries so focus never leaves an open dialog (PaywallModal pattern).
	function wrapTab(event: KeyboardEvent, dialog: HTMLDialogElement | undefined) {
		if (event.key !== 'Tab' || !dialog) return;
		const controls = [...dialog.querySelectorAll<HTMLElement>('a[href], button:not(:disabled)')].filter(
			(control) => control.getClientRects().length > 0
		);
		if (controls.length === 0) return;
		const first = controls[0];
		const last = controls[controls.length - 1];
		if (event.shiftKey && document.activeElement === first) {
			event.preventDefault();
			last.focus();
		} else if (!event.shiftKey && document.activeElement === last) {
			event.preventDefault();
			first.focus();
		}
	}
</script>

<section aria-labelledby="settings-listenbrainz-heading" aria-busy={phase === 'loading' || busy || reauthWorking}>
	<h2 id="settings-listenbrainz-heading" tabindex="-1" bind:this={heading}>ListenBrainz</h2>

	{#if phase === 'loading'}
		{#if showLoading}
			<p class="lb-muted">Loading your ListenBrainz link…</p>
		{/if}
	{:else if phase === 'error'}
		<div class="lb-error-block">
			<Icon name="alert-triangle" size={48} />
			<p><strong>Couldn't load your ListenBrainz link.</strong></p>
			<button type="button" class="lb-button lb-button--ghost" onclick={() => void refresh()}>
				Retry
			</button>
		</div>
	{:else if phase === 'signed_out'}
		<p><a href="/login">Sign in</a> to see your ListenBrainz link.</p>
	{:else}
		{#if phase === 'linked'}
			<p class="lb-linked">
				<Icon name="check" size={20} />
				<span>Linked as <strong>{username}</strong></span>
			</p>
		{:else}
			<p>
				No ListenBrainz account is linked. Linking an account isn't available in NNTune yet.
			</p>
		{/if}
		<p class="lb-muted">
			NNTune doesn't send your listens or feedback to ListenBrainz yet. Your NNTune library, likes
			and history work the same whether or not you link.
		</p>

		{#if accountDeleting}
			<p role="alert" class="lb-error">
				Your account is being deleted, so ListenBrainz can't be changed.
			</p>
		{:else if phase === 'linked'}
			{#if unlinkError}
				<p role="alert" class="lb-error">{unlinkError}</p>
			{/if}
			<button
				type="button"
				class="lb-button lb-button--destructive"
				bind:this={unlinkButton}
				disabled={!online}
				aria-disabled={busy}
				onclick={openConfirm}
			>
				Unlink
			</button>
			{#if !online}
				<p class="lb-muted">You're offline. Connect to change ListenBrainz.</p>
			{/if}
		{/if}
	{/if}

	<p class="lb-sr-only" role="status">{statusMessage}</p>
</section>

<!-- Dialogs exist only once status is known to be linked, so no unlink
     control is in the DOM before then. -->
{#if phase === 'linked'}
<dialog
	bind:this={confirmDialog}
	class="lb-dialog"
	aria-labelledby="lb-unlink-heading"
	aria-describedby="lb-unlink-body"
	oncancel={(event) => {
		event.preventDefault();
		cancelConfirm();
	}}
	onkeydown={(event) => wrapTab(event, confirmDialog)}
>
	<h2 id="lb-unlink-heading">Unlink ListenBrainz?</h2>
	<p id="lb-unlink-body">
		NNTune will delete your saved token. Anything already on ListenBrainz stays there. Your NNTune
		library, likes and history aren't affected.
	</p>
	<div class="lb-actions">
		<button
			type="button"
			class="lb-button lb-button--ghost lb-button--destructive"
			aria-disabled={busy}
			onclick={() => void confirmUnlink()}
		>
			{busy ? 'Unlinking…' : 'Unlink'}
		</button>
		<button type="button" class="lb-button lb-button--ghost" bind:this={cancelButton} aria-disabled={busy} onclick={cancelConfirm}>
			Cancel
		</button>
	</div>
</dialog>

<dialog
	bind:this={reauthDialog}
	class="lb-dialog"
	aria-labelledby="lb-reauth-heading"
	aria-describedby="lb-reauth-body"
	oncancel={(event) => {
		event.preventDefault();
		cancelReauth();
	}}
	onkeydown={(event) => wrapTab(event, reauthDialog)}
>
	<h2 id="lb-reauth-heading" tabindex="-1" bind:this={reauthHeading}>Confirm it's you</h2>
	<p id="lb-reauth-body">To change your ListenBrainz link, confirm with your passkey.</p>
	{#if reauthError}
		<p role="alert" class="lb-error">
			Couldn't confirm it's you. Try again or sign in again.
			<a href="/login">Sign in again</a>
		</p>
	{/if}
	<div class="lb-actions">
		<button
			type="button"
			class="lb-button"
			aria-disabled={reauthWorking}
			onclick={() => void continueWithPasskey()}
		>
			{reauthWorking ? 'Waiting for your passkey…' : 'Continue with passkey'}
		</button>
		<button type="button" class="lb-button lb-button--ghost" onclick={cancelReauth}>Cancel</button>
	</div>
</dialog>
{/if}

<style>
	.lb-muted {
		color: var(--nt-fg-muted);
	}

	.lb-linked {
		display: flex;
		align-items: center;
		gap: var(--nt-space-px-8);
		color: var(--nt-fg);
	}

	.lb-error {
		color: var(--nt-bad-text);
	}

	.lb-error-block {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: var(--nt-space-2);
	}

	.lb-button {
		min-height: var(--nt-control);
		padding-inline: var(--nt-space-4);
		border: 1px solid transparent;
		border-radius: var(--nt-radius-sm);
		background: var(--nt-accent);
		color: var(--nt-accent-fg);
		font: inherit;
		font-weight: 600;
		cursor: pointer;
	}

	.lb-button--ghost {
		background: transparent;
		color: var(--nt-fg);
		border-color: var(--nt-border);
	}

	.lb-button--destructive {
		background: transparent;
		color: var(--nt-bad-text);
		border-color: var(--nt-border);
	}

	.lb-button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.lb-dialog {
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius-dialog);
		background: var(--nt-surface-raised);
		color: var(--nt-fg);
		max-width: min(28rem, calc(100vw - 2 * var(--nt-space-3)));
		padding: var(--nt-space-4);
	}

	.lb-dialog::backdrop {
		background: rgb(0 0 0 / 0.55);
	}

	.lb-dialog h2 {
		margin: 0 0 var(--nt-space-2);
		font-size: 1.25rem;
		line-height: 1.3;
	}

	.lb-actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--nt-space-2);
	}

	.lb-sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		overflow: hidden;
		clip-path: inset(50%);
		white-space: nowrap;
	}
</style>
