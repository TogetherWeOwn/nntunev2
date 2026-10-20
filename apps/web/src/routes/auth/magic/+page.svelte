<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient, ApiError } from '#lib/api/client.js';
	import { invalidateLibraryCache } from '#lib/library/cache.js';
	import {
		clearSignedInEmail,
		readSignedInEmail,
		stashSignedInEmail
	} from '#lib/signed-in-email.js';

	const lifetime = new AbortController();
	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, signal: lifetime.signal });
	let mounted = false;
	let heading: HTMLHeadingElement | undefined;

	let status = $state<'confirm' | 'working' | 'error' | 'signed-out'>('working');
	let token = $state<string | null>(null);
	let redeemError = $state<string | null>(null);
	// Display-only identity of the current session, when this browser has
	// one. Named here so a swap-guarded user knows which account is still
	// signed in and can sign out to redeem the other link.
	let signedInEmail = $state<string | null>(null);
	let signingOut = $state(false);
	// Shown only when POST /v1/session/logout fails: the HttpOnly cookie
	// (the real session) is untouched, so the recovery button must stay.
	let signOutError = $state<string | null>(null);

	// Only a branch change schedules entry focus. Same-branch logout updates
	// must not steal it from a recovery link the user is already interacting with.
	$effect(() => {
		const branch = status;
		void tick().then(() => {
			if (mounted && status === branch) heading?.focus();
		});
	});

	onMount(() => {
		mounted = true;
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
		} else {
			token = captured;
			status = 'confirm';
		}
		return () => {
			mounted = false;
			lifetime.abort();
		};
	});

	async function redeem() {
		if (!mounted || status !== 'confirm' || token === null) return;
		redeemError = null;
		status = 'working';
		try {
			const result = await client.finishMagicLink(token);
			if (!mounted) return;
			invalidateLibraryCache();
			// Show which account this is on the next screen, before any
			// provider credential is typed.
			if (typeof result.email === 'string' && result.email.length > 0) {
				stashSignedInEmail(result.email);
			}
			window.location.href = '/onboarding/provider';
		} catch (error) {
			if (!mounted) return;
			if (error instanceof ApiError && error.paywall) {
				// The token remains available; keep the explicit confirmation
				// action so the user can retry once the global paywall clears.
				status = 'confirm';
				return;
			}
			if (!(error instanceof ApiError) || (error.status >= 500 && error.status < 600)) {
				// Transport failures (including the client's deadline) and server
				// errors are not token rejections. Retry only on another click.
				redeemError = 'Could not reach NNTune. Try again.';
				status = 'confirm';
				return;
			}
			// Generic: expired, already used, unknown and swap-guarded
			// tokens all read the same, like the API. The token is left
			// unspent, so its owner can still redeem it after signing out.
			signedInEmail = readSignedInEmail();
			status = 'error';
		}
	}

	async function signOut() {
		if (!mounted || signingOut) return;
		signingOut = true;
		signOutError = null;
		try {
			await client.logout();
		} catch (error) {
			if (!mounted) return;
			if (!(error instanceof ApiError && error.status === 401)) {
				// The HttpOnly cookie is the real session and it is untouched,
				// so a failed logout must NOT clear the label or show success:
				// keep the button and surface the failure instead.
				signOutError = 'Could not sign out. Try again.';
				signingOut = false;
				return;
			}
			// A generic 401 means there is no live session to revoke.
		}
		if (!mounted) return;
		invalidateLibraryCache();
		clearSignedInEmail();
		signedInEmail = null;
		signingOut = false;
		status = 'signed-out';
	}

	function retryLink() {
		// The token is unspent (swap-guarded finishes never burn it), so
		// after a real sign-out the same link redeems without a new email.
		signOutError = null;
		status = 'confirm';
	}
</script>

<svelte:head>
	<title>NNTune — signing you in</title>
	<meta name="description" content="Redeem your NNTune email sign-in link." />
</svelte:head>

<main class="nt-wrap">
	<h1 bind:this={heading} tabindex="-1">Sign in</h1>
	{#if status === 'confirm' || status === 'working'}
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
		{#if redeemError !== null}
			<p role="alert">{redeemError}</p>
		{/if}
		<button class="nt-button" type="button" onclick={redeem} disabled={status === 'working'}>
			Sign in with this link
		</button>
		{#if status === 'working'}
			<p role="status">Signing you in…</p>
		{:else}
			<p><a href="/login">Cancel — request a new one</a></p>
		{/if}
	{:else if status === 'signed-out'}
		<p role="status">
			Signed out. If that link belongs to another account, open it again now — or
			<a href="/login">request a fresh one</a>.
		</p>
		{#if token !== null}
			<button class="nt-button" type="button" onclick={retryLink}>
				Try this link again
			</button>
		{/if}
	{:else}
		<p role="alert">
			This sign-in link didn't work — it may have expired or already been used.
			{#if signedInEmail !== null}
				You're still signed in as <strong>{signedInEmail}</strong>, so the link was left
				unused: if it belongs to a different account, sign out first and then open it
				again.
			{:else}
				If you are signed in to a different account, sign out first and then open it
				again.
			{/if}
			<a href="/login">Request a new one</a>.
		</p>
		{#if signOutError !== null}
			<p role="alert">{signOutError}</p>
		{/if}
		<button
			class="nt-button nt-button--ghost"
			type="button"
			onclick={signOut}
			disabled={signingOut}
		>
			{signingOut
				? 'Signing out…'
				: signedInEmail !== null
					? `Sign out ${signedInEmail}`
					: 'Sign out and try this link again'}
		</button>
	{/if}
</main>
