<script lang="ts">
	import { onDestroy } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import type { PageProps } from './$types';
	import { ApiClient, ApiError } from '#lib/api/client.js';
	import { notifyLocalAuthTransition } from '#lib/auth-transition.js';
	import { clearSignedInEmail } from '#lib/signed-in-email.js';
	import { invalidateLibraryCache } from '#lib/library/cache.js';

	type Props = { data?: Partial<PageProps['data']> | void };
	let { data = { sessionCheckFailed: false } }: Props = $props();
	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });
	let alive = true;
	onDestroy(() => {
		alive = false;
	});

	/**
	 * A qualifying security result crosses the auth boundary whether this
	 * route is live or abandoned: drop this tab's library snapshots and
	 * identity label, broadcast the transition so other open tabs stop
	 * showing them, and notify mounted same-window consumers. Never
	 * navigates and never throws; every UI/navigation continuation below
	 * stays behind its own liveness guard.
	 */
	function crossAuthBoundary() {
		invalidateLibraryCache();
		clearSignedInEmail();
		notifyLocalAuthTransition();
	}

	function redirectToLogin() {
		if (!alive) return;
		window.location.href = '/login';
	}

	let busy = $state<'local' | 'everywhere' | null>(null);
	let actionError = $state<string | null>(null);
	let revokeIncomplete = $state(false);

	async function signOut() {
		if (!alive || busy !== null) return;
		busy = 'local';
		actionError = null;
		revokeIncomplete = false;
		try {
			await client.logout();
		} catch (error) {
			if (error instanceof ApiError && error.status === 401) {
				// Qualifying 401: this session is confirmed gone. Cross the
				// boundary even when abandoned; navigation stays live-only.
				crossAuthBoundary();
				redirectToLogin();
				return;
			}
			if (!alive) return;
			actionError = 'Could not sign out. Please try again.';
			busy = null;
			return;
		} finally {
			if (!alive) busy = null;
		}
		// Fulfilled success is qualifying live or abandoned.
		crossAuthBoundary();
		redirectToLogin();
	}

	async function signOutEverywhere() {
		if (!alive || busy !== null || !window.confirm('Sign out on every device?')) return;
		busy = 'everywhere';
		actionError = null;
		revokeIncomplete = false;
		try {
			await client.revokeAllSessions();
		} catch (error) {
			if (error instanceof ApiError && error.status === 401) {
				// A 401 means this request was rejected before any session
				// was revoked, so staying here with an explicit message keeps
				// the failure distinguishable from a successful revocation
				// (which navigates to /login with no message). The local
				// session is confirmed gone, so local library snapshots
				// still cross the auth boundary and are invalidated here —
				// even when abandoned, but then with no message or redirect.
				crossAuthBoundary();
				if (!alive) return;
				actionError =
					'Your session expired before other devices were signed out. Sign in again and repeat Sign out everywhere.';
				revokeIncomplete = true;
				busy = null;
				return;
			}
			if (!alive) return;
			actionError = 'Could not confirm sign out everywhere. Please try again.';
			busy = null;
			return;
		} finally {
			if (!alive) busy = null;
		}
		// Fulfilled success is qualifying live or abandoned.
		crossAuthBoundary();
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
		<p role="alert">
			{actionError}
			{#if revokeIncomplete}
				<a href="/login">Sign in again</a>
			{/if}
		</p>
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
