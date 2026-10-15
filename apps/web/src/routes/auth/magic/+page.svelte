<script lang="ts">
	import { onMount } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient } from '#lib/api/client.js';
	import { decodeBase64Url } from '#lib/webauthn.js';

	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });

	let status = $state<'loading' | 'confirm' | 'working' | 'error'>('loading');
	let token = '';
	let accountEmail = $state('');

	onMount(() => {
		// The Worker mails a token and an encoded account address. The API
		// checks that they match before redeeming, so the address is safe to
		// show as the account this link will sign into.
		const params = new URLSearchParams(window.location.search);
		token = params.get('token') ?? '';
		const account = params.get('account') ?? '';
		try {
			accountEmail = new TextDecoder().decode(decodeBase64Url(account));
		} catch {
			accountEmail = '';
		}
		// Remove the bearer token from browser history before rendering it.
		window.history.replaceState(null, '', window.location.pathname);
		status = token.length > 0 && accountEmail.length > 0 ? 'confirm' : 'error';
	});

	async function redeem() {
		if (status !== 'confirm') return;
		status = 'working';
		try {
			await client.finishMagicLink(token, accountEmail);
			sessionStorage.setItem('nntune.signed-in-email', accountEmail);
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
	{#if status === 'loading'}
		<p role="status">Checking sign-in link…</p>
	{:else if status === 'confirm'}
		<p>This link will sign you in as <strong>{accountEmail}</strong>.</p>
		<p role="alert">
			Continuing will replace any account currently signed in on this browser. Only continue if
			you recognize this email. Your provider credentials will be saved to this account.
		</p>
		<button class="nt-button" type="button" onclick={redeem}>Continue to NNTune</button>
	{:else if status === 'working'}
		<p role="status">Signing you in…</p>
	{:else}
		<p role="alert">
			This sign-in link didn't work — it may have expired or already been used.
			<a href="/login">Request a new one</a>.
		</p>
	{/if}
</main>
