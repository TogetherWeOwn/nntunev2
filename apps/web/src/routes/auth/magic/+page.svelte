<script lang="ts">
	import { onMount } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient } from '#lib/api/client.js';

	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });

	let status = $state<'working' | 'error'>('working');

	onMount(() => {
		// The Worker mails `{origin}/auth/magic?token=…` (worker_entry.rs);
		// this page is the landing spot that redeems it. The token is
		// single-use, so redeem exactly once, on mount.
		const token = new URLSearchParams(window.location.search).get('token');
		// Drop the token from the address bar/history before redeeming, so
		// it isn't left behind in browser history or a copied URL.
		window.history.replaceState(null, '', window.location.pathname);
		if (token === null || token.length === 0) {
			status = 'error';
			return;
		}
		void (async () => {
			try {
				await client.finishMagicLink(token);
				window.location.href = '/onboarding/provider';
			} catch {
				// Generic: expired, already used and unknown tokens all read
				// the same, like the API.
				status = 'error';
			}
		})();
	});
</script>

<svelte:head>
	<title>NNTune — signing you in</title>
	<meta name="description" content="Redeem your NNTune email sign-in link." />
</svelte:head>

<main class="nt-wrap">
	<h1>Sign in</h1>
	{#if status === 'working'}
		<p role="status">Signing you in…</p>
	{:else}
		<p role="alert">
			This sign-in link didn't work — it may have expired or already been used.
			<a href="/login">Request a new one</a>.
		</p>
	{/if}
</main>
