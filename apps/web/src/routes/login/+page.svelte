<script lang="ts">
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { invalidateLibraryCache } from '#lib/library/cache.js';
	import { ApiClient, ApiError } from '#lib/api/client.js';
	import { stashSignedInEmail } from '#lib/signed-in-email.js';
	import { serializePublicKeyCredential, toRequestOptions } from '#lib/webauthn.js';

	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });

	let email = $state('');
	let status = $state<'idle' | 'sending' | 'sent' | 'error'>('idle');
	let passkeyStatus = $state<
		'idle' | 'working' | 'unsupported' | 'dismissed' | 'error' | 'email-required'
	>('idle');

	const passkeysSupported =
		typeof window !== 'undefined' &&
		typeof window.PublicKeyCredential !== 'undefined';

	async function sendMagicLink(event: SubmitEvent) {
		event.preventDefault();
		if (status === 'sending') return;
		status = 'sending';
		try {
			await client.requestMagicLink(email.trim());
			// Generic either way: the API never reveals which emails exist.
			status = 'sent';
		} catch (error) {
			status = error instanceof ApiError && error.paywall ? 'idle' : 'error';
		}
	}

	/**
	 * Retryable ceremony interruption: `NotAllowedError` / `AbortError`
	 * can mean dismissal, timeout, or an unavailable passkey. The name
	 * alone cannot identify the cause. Server (`ApiError`) paths never
	 * carry these names, so login stays non-enumerating.
	 */
	function isPasskeyDismissal(error: unknown): boolean {
		const name = (error as { name?: unknown } | null)?.name;
		return name === 'NotAllowedError' || name === 'AbortError';
	}

	async function signInWithPasskey() {
		if (passkeyStatus === 'working') return;
		const attemptedEmail = email.trim();
		if (attemptedEmail.length === 0) {
			passkeyStatus = 'email-required';
			return;
		}
		if (!passkeysSupported) {
			passkeyStatus = 'unsupported';
			return;
		}
		passkeyStatus = 'working';
		try {
			// Email-first login: the API answers the same shape for known
			// and unknown addresses (never an oracle) with an empty
			// allow-list, and the browser offers any local credential.
			const options = await client.loginStart(attemptedEmail);
			const assertion = await navigator.credentials.get({
				publicKey: toRequestOptions({
					challenge: options.challenge,
					rpId: options.rp_id,
					allowCredentials: []
				})
			});
			if (!(assertion instanceof PublicKeyCredential)) {
				throw new Error('no public-key credential');
			}
			const serialized = serializePublicKeyCredential(assertion);
			const response = serialized.response as Record<string, unknown>;
			const authenticatorData = response.authenticatorData;
			const clientDataJSON = response.clientDataJSON;
			const signature = response.signature;
			const credentialId = serialized.id;
			if (
				typeof authenticatorData !== 'string' ||
				typeof clientDataJSON !== 'string' ||
				typeof signature !== 'string' ||
				typeof credentialId !== 'string'
			) {
				throw new Error('credential missing assertion fields');
			}
			// Exact finish shape (the API rejects unknown fields).
			await client.loginFinish({
				challenge: options.challenge,
				credentialId,
				authenticatorData,
				clientDataJson: clientDataJSON,
				signature
			});
			stashSignedInEmail(attemptedEmail);
			invalidateLibraryCache();
			window.location.href = '/onboarding/provider';
		} catch (error) {
			if (error instanceof ApiError && error.paywall) {
				passkeyStatus = 'idle';
				return;
			}
			if (isPasskeyDismissal(error)) {
				// Interrupted ceremony: same possible causes for every
				// address, so login stays non-enumerating.
				passkeyStatus = 'dismissed';
				return;
			}
			// Generic: login errors stay non-enumerating, like the API.
			passkeyStatus = 'error';
		}
	}
</script>

<svelte:head>
	<title>NNTune — sign in</title>
	<meta name="description" content="Sign in to NNTune with a passkey or an email magic link." />
</svelte:head>

<main class="nt-wrap">
	<h1>Sign in</h1>

	<section aria-labelledby="passkey-heading">
		<h2 id="passkey-heading">Passkey</h2>
		<p>Enter your email below, then use your passkey.</p>
		<button
			class="nt-button"
			type="button"
			onclick={signInWithPasskey}
			disabled={passkeyStatus === 'working'}
		>
			Sign in with passkey
		</button>
		{#if passkeyStatus === 'working'}
			<p role="status">Signing in with your passkey…</p>
		{:else if passkeyStatus === 'email-required'}
			<p role="status">Enter your email to continue with passkey sign-in.</p>
		{:else if passkeyStatus === 'unsupported'}
			<p role="status">This browser does not support passkeys — use an email link instead.</p>
		{:else if passkeyStatus === 'dismissed'}
			<p role="alert">
				Passkey sign-in didn't complete. Possible reasons: You dismissed the passkey prompt,
				it timed out, or no usable passkey was available. Try again or use an email link.
			</p>
		{:else if passkeyStatus === 'error'}
			<p role="alert">Sign-in failed. Try again or use an email link.</p>
		{/if}
	</section>

	<section aria-labelledby="magic-link-heading">
		<h2 id="magic-link-heading">Email link</h2>
		<form onsubmit={sendMagicLink}>
			<label>
				Email
				<input
					type="email"
					name="email"
					required
					autocomplete="email"
					bind:value={email}
					disabled={status === 'sending' || passkeyStatus === 'working'}
				/>
			</label>
			<button class="nt-button nt-button--ghost" type="submit" disabled={status === 'sending'}>
				{status === 'sending' ? 'Sending…' : 'Email me a sign-in link'}
			</button>
		</form>
		{#if status === 'sent'}
			<p role="status">Check your inbox for a sign-in link.</p>
		{:else if status === 'error'}
			<p role="alert">Something went wrong. Try again.</p>
		{/if}
	</section>

	<p>New here? <a href="/signup">Create an account</a>.</p>
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
