<script lang="ts">
	import { onMount } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient } from '#lib/api/client.js';
	import {
		clearSignedInEmail,
		readSignedInEmail,
		stashSignedInEmail
	} from '#lib/signed-in-email.js';

	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });

	let status = $state<'confirm' | 'working' | 'error' | 'signed-out'>('working');
	let token = $state<string | null>(null);
	// Display-only identity of the current session, when this browser has
	// one. Named here so a swap-guarded user knows which account is still
	// signed in and can sign out to redeem the other link.
	let signedInEmail = $state<string | null>(null);
	let signingOut = $state(false);

	onMount(() => {
		// The Worker mails `{origin}/auth/magic?token=…` (worker_entry.rs).
		// Nothing is redeemed automatically: an auto-redeem would let
		// anyone who can get a victim to open a link sign the victim into
		// the sender's account (login CSRF). The token is captured,
		// scrubbed from the address bar/history, and only spent on an
		// explicit confirm click below.
		signedInEmail = readSignedInEmail();
		const captured = new URLSearchParams(window.location.search).get('token');
		window.history.replaceState(null, '', window.location.pathname);
		if (captured === null || captured.length === 0) {
			status = 'error';
			return;
		}
		token = captured;
		status = 'confirm';
	});

	async function redeem() {
		if (status !== 'confirm' || token === null) return;
		status = 'working';
		try {
			const result = await client.finishMagicLink(token);
			// Show which account this is on the next screen, before any
			// provider credential is typed.
			if (typeof result.email === 'string' && result.email.length > 0) {
				stashSignedInEmail(result.email);
			}
			window.location.href = '/onboarding/provider';
		} catch {
			// Generic: expired, already used, unknown and swap-guarded
			// tokens all read the same, like the API. The token is left
			// unspent, so its owner can still redeem it after signing out.
			signedInEmail = readSignedInEmail();
			status = 'error';
		}
	}

	async function signOut() {
		if (signingOut) return;
		signingOut = true;
		try {
			await client.logout();
		} catch {
			// The label is display-only: even a failed logout clears it so
			// a stranded session can always be abandoned locally. The
			// HttpOnly cookie itself is revoked server-side when reachable.
		}
		clearSignedInEmail();
		signedInEmail = null;
		signingOut = false;
		status = 'signed-out';
	}
</script>

<svelte:head>
	<title>NNTune — signing you in</title>
	<meta name="description" content="Redeem your NNTune email sign-in link." />
</svelte:head>

<main class="nt-wrap">
	<h1>Sign in</h1>
	{#if status === 'confirm'}
		{#if signedInEmail !== null}
			<p role="status">
				You're currently signed in as <strong>{signedInEmail}</strong>. This link signs
				you into a different account only after you confirm — if you didn't ask for it,
				don't continue.
			</p>
		{/if}
		<p role="status">
			Sign in with this email link? Only continue if you just asked NNTune to email you a
			sign-in link. If someone sent you this link out of the blue, don't continue — you'd be
			signed into <em>their</em> account, not yours, and anything you enter next would be
			saved there.
		</p>
		<button class="nt-button" type="button" onclick={redeem}>Sign in with this link</button>
		<p><a href="/login">Cancel — request a new one</a></p>
	{:else if status === 'working'}
		<p role="status">Signing you in…</p>
	{:else if status === 'signed-out'}
		<p role="status">
			Signed out. If that link belongs to another account, open it again now — or
			<a href="/login">request a fresh one</a>.
		</p>
	{:else}
		<p role="alert">
			This sign-in link didn't work — it may have expired or already been used.
			{#if signedInEmail !== null}
				You're still signed in as <strong>{signedInEmail}</strong>, so the link was left
				unused: if it belongs to a different account, sign out first and then open it
				again.
			{/if}
			<a href="/login">Request a new one</a>.
		</p>
		{#if signedInEmail !== null}
			<button
				class="nt-button nt-button--ghost"
				type="button"
				onclick={signOut}
				disabled={signingOut}
			>
				{signingOut ? 'Signing out…' : `Sign out ${signedInEmail}`}
			</button>
		{/if}
	{/if}
</main>
