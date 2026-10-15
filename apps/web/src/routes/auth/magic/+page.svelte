<script lang="ts">
	import { onMount } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient } from '#lib/api/client.js';
	import { stashSignedInEmail } from '#lib/signed-in-email.js';

	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });

	let status = $state<'confirm' | 'working' | 'error'>('working');
	let token = $state<string | null>(null);

	onMount(() => {
		// The Worker mails `{origin}/auth/magic?token=…` (worker_entry.rs).
		// Nothing is redeemed automatically: an auto-redeem would let
		// anyone who can get a victim to open a link sign the victim into
		// the sender's account (login CSRF). The token is captured,
		// scrubbed from the address bar/history, and only spent on an
		// explicit confirm click below.
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
			// Generic: expired, already used and unknown tokens all read
			// the same, like the API.
			status = 'error';
		}
	}
</script>

<svelte:head>
	<title>NNTune — signing you in</title>
	<meta name="description" content="Redeem your NNTune email sign-in link." />
</svelte:head>

<main class="nt-wrap">
	<h1>Sign in</h1>
	{#if status === 'confirm'}
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
	{:else}
		<p role="alert">
			This sign-in link didn't work — it may have expired or already been used.
			<a href="/login">Request a new one</a>.
		</p>
	{/if}
</main>
