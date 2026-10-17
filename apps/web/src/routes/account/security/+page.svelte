<script lang="ts">
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import type { PageProps } from './$types';
	import { ApiClient, ApiError } from '#lib/api/client.js';
	import { clearSignedInEmail } from '#lib/signed-in-email.js';

	type Props = { data?: Partial<PageProps['data']> | void };
	let { data = { sessionCheckFailed: false } }: Props = $props();
	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });

	function redirectToLogin() {
		clearSignedInEmail();
		window.location.href = '/login';
	}

	let busy = $state<'local' | 'everywhere' | null>(null);
	let actionError = $state<string | null>(null);

	async function signOut() {
		if (busy !== null) return;
		busy = 'local';
		actionError = null;
		try {
			await client.logout();
		} catch (error) {
			if (error instanceof ApiError && error.status === 401) {
				redirectToLogin();
				return;
			}
			actionError = 'Could not sign out. Please try again.';
			busy = null;
			return;
		}
		redirectToLogin();
	}

	async function signOutEverywhere() {
		if (busy !== null || !window.confirm('Sign out on every device?')) return;
		busy = 'everywhere';
		actionError = null;
		try {
			await client.revokeAllSessions();
		} catch (error) {
			if (error instanceof ApiError && error.status === 401) {
				redirectToLogin();
				return;
			}
			actionError = 'Could not confirm sign out everywhere. Please try again.';
			busy = null;
			return;
		}
		redirectToLogin();
	}
</script>

<svelte:head>
	<title>Account security — NNTune v2</title>
	<meta name="description" content="Manage your NNTune sign-in sessions." />
</svelte:head>

<main>
	<h1>Account security</h1>
	<p>Sign out of this browser or end every active session on your account.</p>

	{#if data.sessionCheckFailed}
		<p role="status">Could not verify your sign-in status. Sign-out actions may fail until the API is reachable.</p>
	{/if}

	{#if actionError}
		<p role="alert">{actionError}</p>
	{/if}

	<div class="actions">
		<button type="button" onclick={signOut} disabled={busy !== null}>
			{busy === 'local' ? 'Signing out…' : 'Sign out'}
		</button>
		<button type="button" onclick={signOutEverywhere} disabled={busy !== null}>
			{busy === 'everywhere' ? 'Signing out everywhere…' : 'Sign out everywhere'}
		</button>
	</div>
</main>

<style>
	main {
		max-width: 44rem;
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem;
		margin-top: 1rem;
	}
</style>
