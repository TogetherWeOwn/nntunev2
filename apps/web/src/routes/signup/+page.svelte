<script lang="ts">
	import { onDestroy, tick } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { afterNavigate, goto } from '$app/navigation';
	import { page } from '$app/state';
	import { invalidateLibraryCache } from '#lib/library/cache.js';
	import { ApiClient, ApiError, isMemberInviteCode, type RegisterStartResponse } from '#lib/api/client.js';
	import { stashSignedInEmail } from '#lib/signed-in-email.js';
	import {
		serializePublicKeyCredential,
		toCreationOptions,
		toRegistrationOptions
	} from '#lib/webauthn.js';

	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });
	const maxEmailLength = 254;
	const maxCodeLength = 256;
	// Teardown is permanent even when a transport or authenticator ignores abort.
	const attempt = new AbortController();
	onDestroy(() => attempt.abort());
	let emailInput = $state<HTMLInputElement>();
	let codeInput = $state<HTMLInputElement>();

	let email = $state('');
	let magicCode = $state('');
	let inviteCode = $state('');
	let inviteFromLink = $state(false);
	let privateListening = $state(false);
	let pending: RegisterStartResponse | null = $state(null);
	let status = $state<
		| 'idle'
		| 'working'
		| 'code-sent'
		| 'finishing'
		| 'code-error'
		| 'dismissed'
		| 'created'
		| 'unsupported'
		| 'error'
	>('idle');

	const finishLocked = $derived(status === 'finishing' || status === 'created');
	const passkeysSupported =
		typeof window !== 'undefined' &&
		typeof window.PublicKeyCredential !== 'undefined';

	async function captureInvite() {
		const url = new URL(window.location.href);
		const params = new URLSearchParams(url.hash.slice(1));
		if (!params.has('invite')) return;
		const codes = params.getAll('invite');
		// A shallow replace retains the old URL in router history state. Use a
		// regular same-route replacement so neither the address nor state keeps it.
		url.hash = '';
		try {
			await goto(url, { replace: true, reset: false, state: { ...page.state } });
		} catch {
			return;
		}
		// Sending an email code may validate the invite. Only enrollment spends it.
		if (!attempt.signal.aborted && window.location.href === url.href &&
			!finishLocked && codes.length === 1 && isMemberInviteCode(codes[0])) {
			inviteCode = codes[0];
			inviteFromLink = true;
		}
	}

	afterNavigate(() => { void captureInvite(); });

	function validInvite(code: string): boolean {
		return code.length <= maxCodeLength && !/[\s\p{Cc}\p{Cs}]/u.test(code);
	}

	async function startAccount(event: SubmitEvent) {
		event.preventDefault();
		const attemptedEmail = email.trim();
		const attemptedInvite = inviteCode.trim();
		if (
			attempt.signal.aborted ||
			status === 'working' ||
			status === 'finishing' ||
			!attemptedEmail || attemptedEmail.length > maxEmailLength || !emailInput?.reportValidity()
		) return;
		if (!validInvite(attemptedInvite)) {
			status = 'error';
			return;
		}
		if (!passkeysSupported) {
			status = 'unsupported';
			return;
		}
		status = 'working';
		try {
			// The API answers the same shape for every email (never an
			// existence oracle) and mails an inbox-proof code when the
			// address can still enroll.
			const options = await client.registerStart(attemptedEmail, attempt.signal, attemptedInvite || undefined);
			if (attempt.signal.aborted) return;
			pending = options;
			status = 'code-sent';
			await tick();
			if (attempt.signal.aborted) return;
			codeInput?.focus();
		} catch (error) {
			if (attempt.signal.aborted) return;
			if (error instanceof ApiError && error.paywall) {
				status = 'idle';
				return;
			}
			// Generic: account creation never says which step failed.
			status = 'error';
		}
	}

	/**
	 * Retryable ceremony interruption: `NotAllowedError` / `AbortError`
	 * can mean dismissal, timeout, or an unavailable authenticator. No
	 * finish call was sent, so the code and challenge are unconsumed
	 * (their expiry still applies). Server (`ApiError`) paths never
	 * carry these names.
	 */
	function isPasskeyDismissal(error: unknown): boolean {
		const name = (error as { name?: unknown } | null)?.name;
		return name === 'NotAllowedError' || name === 'AbortError';
	}

	async function startOver() {
		if (attempt.signal.aborted || status === 'working' || status === 'finishing') return;
		// Back to the email step with a clean slate: the recovery path
		// when a failure after the single-use claims (expired challenge,
		// duplicate credential) means this attempt can never succeed.
		pending = null;
		magicCode = '';
		inviteCode = '';
		inviteFromLink = false;
		status = 'idle';
		await tick();
		if (attempt.signal.aborted) return;
		emailInput?.focus();
	}

	async function finishAccount(event: SubmitEvent) {
		event.preventDefault();
		if (attempt.signal.aborted || pending === null || finishLocked) return;
		const { challenge, rp_id } = pending;
		const attemptedEmail = email.trim();
		const codes = {
			magicToken: magicCode.trim(),
			...(inviteCode.trim().length > 0 ? { inviteCode: inviteCode.trim() } : {})
		};
		if (!/^[A-Za-z0-9_-]{1,256}$/.test(codes.magicToken) || !validInvite(codes.inviteCode ?? '')) {
			status = 'code-error';
			return;
		}
		status = 'finishing';
		try {
			// The server returns only {challenge, user_id, rp_id}; the full
			// creation options are assembled client-side (see
			// toRegistrationOptions).
			const credential = await navigator.credentials.create({
				signal: attempt.signal,
				publicKey: toCreationOptions(
					toRegistrationOptions(challenge, rp_id, attemptedEmail)
				)
			});
			if (attempt.signal.aborted) return;
			if (!(credential instanceof PublicKeyCredential)) {
				throw new Error('no public-key credential');
			}
			const serialized = serializePublicKeyCredential(credential);
			const response = serialized.response as Record<string, unknown>;
			const attestationObject = response.attestationObject;
			const clientDataJSON = response.clientDataJSON;
			const credentialId = serialized.id;
			if (
				typeof attestationObject !== 'string' ||
				typeof clientDataJSON !== 'string' ||
				typeof credentialId !== 'string'
			) {
				throw new Error('credential missing attestation fields');
			}
			// Exact finish shape (the API rejects unknown fields): the
			// emailed code proves inbox control before the key enrolls.
			const result = await client.registerFinish(
				{
					challenge,
					credentialId,
					attestationObject,
					clientDataJson: clientDataJSON,
					...codes
				},
				attempt.signal
			);
			if (attempt.signal.aborted) return;
			if (result?.ok !== true || (result.private_listening !== undefined && result.private_listening !== true)) {
				throw new Error('Invalid account creation result');
			}
			privateListening = result.private_listening === true;
			const purge = invalidateLibraryCache();
			status = 'created';
			stashSignedInEmail(attemptedEmail);
			await purge;
			if (!attempt.signal.aborted) {
				if (privateListening) window.location.href = '/library';
				else window.location.href = '/account/billing';
			}
		} catch (error) {
			if (attempt.signal.aborted) return;
			if (error instanceof ApiError && error.paywall) {
				// Keep the retryable code form and single-use challenge available.
				status = 'code-sent';
				return;
			}
			if (isPasskeyDismissal(error)) {
				// Interrupted before any finish call: keep the typed code
				// and the pending challenge so the user retries in place.
				status = 'dismissed';
				return;
			}
			// Generic: account creation never says which step failed. Stay
			// on the code step: a mistyped code is rejected before the
			// challenge is spent, so the same emailed code can simply be
			// re-entered without asking for another email.
			status = 'code-error';
		}
	}
</script>

<svelte:window onhashchange={captureInvite} />

<svelte:head>
	<title>NNTune — get started</title>
	<meta name="description" content="Create your NNTune account with a passkey. If you need a subscription, choose an account plan after signup." />
</svelte:head>

<main class="nt-wrap">
	<h1>Get started</h1>
	<p>
		Create your account with a passkey. If you received an invite, enter its code before requesting an email code.
	</p>
	{#if status === 'code-sent' || status === 'finishing' || status === 'code-error' || status === 'dismissed' || status === 'created'}
		<form method="post" onsubmit={finishAccount}>
			<p id="signup-code-guidance" role="status">
				We emailed a sign-in link and a code to {email.trim()} — enter the code here to
				prove the address is yours, then touch your security key when asked. (Don't use
				the link for this step; it signs you in instead.)
			</p>
			{#if status === 'dismissed'}
				<p id="signup-dismissal" role="alert">
					Passkey creation didn't complete. Possible reasons: You dismissed the passkey prompt,
					it timed out, or no authenticator was available. Your code is still valid until it
					expires. Try again when you're ready.
				</p>
			{/if}
			{#if status === 'code-error'}
				<p id="signup-code-error" role="alert">
					That didn't work — re-enter the code (and invite code, if you have one) from the
					email and try again. If it keeps failing, start over to get a fresh code.
				</p>
			{/if}
			{#if status === 'code-sent' || status === 'dismissed' || status === 'code-error'}
				<button class="nt-button nt-button--ghost" type="button" onclick={startOver}>
					Start over
				</button>
			{/if}
			<label>
				Email code
				<input
					type="text"
					name="magic_code"
					required
					maxlength={maxCodeLength}
					autocomplete="one-time-code"
					bind:this={codeInput}
					aria-invalid={status === 'code-error'}
					aria-describedby={`signup-code-guidance${status === 'code-error' ? ' signup-code-error' : status === 'dismissed' ? ' signup-dismissal' : ''}`}
					bind:value={magicCode}
					disabled={finishLocked}
				/>
			</label>
			<label>
				Invite code (only if you were given one)
				<input
					type="text"
					name="invite_code"
					maxlength={maxCodeLength}
					autocomplete="off"
					aria-describedby={status === 'code-error' ? 'signup-code-error' : undefined}
					bind:value={inviteCode}
					disabled={finishLocked}
				/>
			</label>
			<button class="nt-button" type="submit" disabled={finishLocked}>
				{status === 'finishing' ? 'Creating…' : 'Finish creating account'}
			</button>
		</form>
	{:else}
		{#if inviteFromLink}<p role="status">Invite link added. Enter your email to get started.</p>{/if}
		<form method="post" onsubmit={startAccount}>
			<label for="signup-email">
				Email
				<input
					id="signup-email"
					data-testid="signup-email"
					type="email"
					name="email"
					required
					maxlength={maxEmailLength}
					autocomplete="username"
					bind:this={emailInput}
					aria-invalid={status === 'error'}
					aria-describedby={status === 'error' ? 'signup-email-error' : undefined}
					bind:value={email}
					disabled={status === 'working'}
				/>
			</label>
			<label for="signup-invite">
				Invite code (if you have one)
				<input id="signup-invite" type="text" autocomplete="off" maxlength={maxCodeLength} bind:value={inviteCode}
					oninput={() => { inviteFromLink = false; }}
					disabled={status === 'working'} />
			</label>
			<button
				data-testid="signup-submit"
				class="nt-button"
				type="submit"
				disabled={status === 'working'}
			>
				{status === 'working' ? 'Creating…' : 'Create account with passkey'}
			</button>
		</form>
	{/if}
	{#if status === 'working'}
		<p role="status">Sending your email code…</p>
	{:else if status === 'finishing'}
		<p role="status">Creating your passkey and finishing account setup…</p>
	{:else if status === 'unsupported'}
		<p role="status">
			This browser does not support passkeys. <a href="/login">Sign in with an email link</a>
			instead.
		</p>
	{:else if status === 'error'}
		<p id="signup-email-error" data-testid="signup-error" role="alert">Something went wrong. Try again.</p>
	{:else if status === 'created'}
		<p role="status">{privateListening ? 'Account created. Opening Library…' : 'Account created — taking you to choose your plan…'}</p>
	{/if}
	<p>Already have an account? <a href="/login">Sign in</a>.</p>
</main>

<style>
	form {
		display: grid;
		gap: var(--nt-space-2);
		max-width: 28rem;
		margin-block: var(--nt-space-2);
	}
	label {
		display: grid;
		gap: var(--nt-space-1);
		font-weight: 600;
	}
	input {
		padding: var(--nt-space-2);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
		font: inherit;
	}
</style>
