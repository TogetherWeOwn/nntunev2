<script lang="ts">
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { broadcastAuthTransition, clearSnapshot } from '#lib/library/cache.js';
	import { ApiClient, type RegisterStartResponse } from '#lib/api/client.js';
	import { stashSignedInEmail } from '#lib/signed-in-email.js';
	import {
		serializePublicKeyCredential,
		toCreationOptions,
		toRegistrationOptions
	} from '#lib/webauthn.js';

	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });

	let email = $state('');
	let magicCode = $state('');
	let inviteCode = $state('');
	let pending: RegisterStartResponse | null = $state(null);
	let status = $state<
		| 'idle'
		| 'working'
		| 'code-sent'
		| 'finishing'
		| 'code-error'
		| 'created'
		| 'unsupported'
		| 'error'
	>('idle');

	const passkeysSupported =
		typeof window !== 'undefined' &&
		typeof window.PublicKeyCredential !== 'undefined';

	async function startAccount(event: SubmitEvent) {
		event.preventDefault();
		if (status === 'working' || status === 'finishing' || email.trim().length === 0) return;
		if (!passkeysSupported) {
			status = 'unsupported';
			return;
		}
		status = 'working';
		try {
			// The API answers the same shape for every email (never an
			// existence oracle) and mails an inbox-proof code when the
			// address can still enroll.
			pending = await client.registerStart(email.trim());
			status = 'code-sent';
		} catch {
			// Generic: account creation never says which step failed.
			status = 'error';
		}
	}

	function startOver() {
		// Back to the email step with a clean slate: the recovery path
		// when a failure after the single-use claims (expired challenge,
		// duplicate credential) means this attempt can never succeed.
		pending = null;
		magicCode = '';
		inviteCode = '';
		status = 'idle';
	}

	async function finishAccount(event: SubmitEvent) {
		event.preventDefault();
		if (pending === null || status === 'finishing' || magicCode.trim().length === 0) return;
		status = 'finishing';
		try {
			// The server returns only {challenge, user_id, rp_id}; the full
			// creation options are assembled client-side (see
			// toRegistrationOptions).
			const credential = await navigator.credentials.create({
				publicKey: toCreationOptions(
					toRegistrationOptions(pending.challenge, pending.user_id, pending.rp_id, email.trim())
				)
			});
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
			await client.registerFinish({
				challenge: pending.challenge,
				credentialId,
				attestationObject,
				clientDataJson: clientDataJSON,
				magicToken: magicCode.trim(),
				...(inviteCode.trim().length > 0 ? { inviteCode: inviteCode.trim() } : {})
			});
			clearSnapshot(window.sessionStorage);
			clearSnapshot(window.localStorage);
			broadcastAuthTransition(window.localStorage);
			status = 'created';
			stashSignedInEmail(email.trim());
			window.location.href = '/onboarding/byop';
		} catch {
			// Generic: account creation never says which step failed. Stay
			// on the code step: a mistyped code is rejected before the
			// challenge is spent, so the same emailed code can simply be
			// re-entered without asking for another email.
			status = 'code-error';
		}
	}
</script>

<svelte:head>
	<title>NNTune — get started</title>
	<meta name="description" content="Create your NNTune account and connect your Usenet provider." />
</svelte:head>

<main class="nt-wrap">
	<h1>Get started</h1>
	<p>
		Create your account with a passkey — no password to phish or leak. Then connect your own
		Usenet provider in the next step.
	</p>
	{#if status === 'code-sent' || status === 'finishing' || status === 'code-error' || status === 'created'}
		<form onsubmit={finishAccount}>
			<p role="status">
				We emailed a sign-in link and a code to {email.trim()} — enter the code here to
				prove the address is yours, then touch your security key when asked. (Don't use
				the link for this step; it signs you in instead.)
			</p>
			{#if status === 'code-error'}
				<p role="alert">
					That didn't work — re-enter the code (and invite code, if you have one) from the
					email and try again. If it keeps failing, start over to get a fresh code.
				</p>
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
					autocomplete="one-time-code"
					bind:value={magicCode}
					disabled={status === 'finishing'}
				/>
			</label>
			<label>
				Invite code (only if you were given one)
				<input
					type="text"
					name="invite_code"
					autocomplete="off"
					bind:value={inviteCode}
					disabled={status === 'finishing'}
				/>
			</label>
			<button class="nt-button" type="submit" disabled={status === 'finishing'}>
				{status === 'finishing' ? 'Creating…' : 'Finish creating account'}
			</button>
		</form>
	{:else}
		<form onsubmit={startAccount}>
			<label>
				Email
				<input
					data-testid="signup-email"
					type="email"
					name="email"
					required
					autocomplete="email"
					bind:value={email}
					disabled={status === 'working'}
				/>
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
	{#if status === 'unsupported'}
		<p role="status">
			This browser does not support passkeys. <a href="/login">Sign in with an email link</a>
			instead.
		</p>
	{:else if status === 'error'}
		<p data-testid="signup-error" role="alert">Something went wrong. Try again.</p>
	{:else if status === 'created'}
		<p role="status">Account created — taking you to provider setup…</p>
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
