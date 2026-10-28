<script lang="ts">
	import { onMount } from 'svelte';
	import { beforeNavigate } from '$app/navigation';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient, ApiError, type SavedProviderSummary } from '#lib/api/client.js';
	import { subscribeLocalAuthTransition } from '#lib/auth-transition.js';
	import { LIBRARY_AUTH_TRANSITION_KEY, browserStorage, clearSnapshot, invalidateLibraryCache } from '#lib/library/cache.js';
	import { clearSignedInEmail } from '#lib/signed-in-email.js';
	import { serializePublicKeyCredential, toRequestOptions } from '#lib/webauthn.js';

	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });
	let summary = $state<SavedProviderSummary | null>(null);
	let busy = $state<'loading' | 'testing' | 'removing' | 'reauth' | null>('loading');
	let expired = $state(false);
	let errorMessage = $state('');
	let notice = $state('');
	let testMessage = $state('');
	let reauthRequired = $state(false);
	let active = false;
	let generation = 0;
	let controller: AbortController | null = null;
	const hasSettings = $derived(summary !== null && (summary.provider_configured || summary.indexer_configured));

	function invalidate() {
		generation += 1;
		controller?.abort();
		controller = null;
		summary = null;
		testMessage = '';
		notice = '';
		errorMessage = '';
		reauthRequired = false;
		busy = null;
	}

	function sessionExpired(broadcast = true) {
		invalidate();
		expired = true;
		clearSignedInEmail();
		if (broadcast) invalidateLibraryCache();
		else clearSnapshot(browserStorage('session'));
	}

	function leavePage() {
		active = false;
		invalidate();
	}

	beforeNavigate(() => leavePage());

	onMount(() => {
		active = true;
		const authTransition = (event: StorageEvent) => {
			// Consume the transition, never relay it back to other tabs.
			if (event.key === LIBRARY_AUTH_TRANSITION_KEY || event.key === null) sessionExpired(false);
		};
		const restorePage = (event: PageTransitionEvent) => {
			if (!event.persisted) return;
			active = true;
			if (!expired) void loadSummary();
		};
		// Same-window transitions share the delivered-event reset: expire the
		// summary without rebroadcasting, requesting or navigating.
		const unsubscribeLocal = subscribeLocalAuthTransition(() => sessionExpired(false));
		window.addEventListener('storage', authTransition);
		window.addEventListener('pagehide', leavePage);
		window.addEventListener('pageshow', restorePage);
		void loadSummary();
		return () => {
			leavePage();
			unsubscribeLocal();
			window.removeEventListener('storage', authTransition);
			window.removeEventListener('pagehide', leavePage);
			window.removeEventListener('pageshow', restorePage);
		};
	});

	function begin(operation: NonNullable<typeof busy>) {
		controller?.abort();
		controller = new AbortController();
		busy = operation;
		errorMessage = '';
		notice = '';
		return { id: ++generation, signal: controller.signal };
	}

	function current(id: number) {
		return active && !expired && id === generation;
	}

	function finish(id: number) {
		if (current(id)) {
			busy = null;
			controller = null;
		}
	}

	function showFailure(error: unknown, fallback: string) {
		// Do not echo server messages, credential error details or response bodies.
		if (error instanceof ApiError) {
			if (error.code === 'reauth_required') {
				reauthRequired = true;
				errorMessage = 'Your session is too old for this action. Reauthenticate with your passkey, then choose the action again.';
				return;
			}
			if (error.status === 401) {
				sessionExpired();
				return;
			}
			if (error.paywall) {
				notice = 'The request was limited. No success was confirmed. Check your plan or try again later.';
				return;
			}
			if (error.status === 403) {
				errorMessage = 'This action is not permitted for your account.';
				return;
			}
			if (error.status === 404) {
				errorMessage = 'The saved-provider service is unavailable. Try again later.';
				return;
			}
		}
		errorMessage = fallback;
	}

	async function loadSummary() {
		if (!active || expired || (busy !== null && busy !== 'loading')) return;
		const { id, signal } = begin('loading');
		summary = null;
		testMessage = '';
		try {
			const result = await client.getSavedProviderSummary(signal);
			if (current(id)) summary = result;
		} catch (error) {
			if (current(id)) showFailure(error, 'Could not load saved-provider status. Try again.');
		} finally {
			finish(id);
		}
	}

	async function testSavedProvider() {
		if (!active || expired || busy !== null || !summary?.provider_configured) return;
		const { id, signal } = begin('testing');
		testMessage = '';
		try {
			const result = await client.testSavedProviderConnection(signal);
			if (!current(id)) return;
			testMessage = result.ok
				? 'Reachability test passed. The saved provider is reachable.'
				: 'Reachability test failed. The server could not reach the saved provider.';
		} catch (error) {
			if (current(id)) showFailure(error, 'Reachability test failed. Try again when the service is available.');
		} finally {
			finish(id);
		}
	}

	async function removeSavedProvider() {
		if (!active || expired || busy !== null || !hasSettings) return;
		if (!window.confirm('Remove the saved provider and indexer settings from your vault? Existing connections may continue until they end.')) {
			notice = 'Removal cancelled. Saved settings were not changed.';
			return;
		}
		const { id, signal } = begin('removing');
		testMessage = '';
		try {
			const result = await client.deleteSavedProviderConfig(signal);
			if (!current(id)) return;
			summary = result;
			reauthRequired = false;
			notice = 'Saved provider and indexer settings removed.';
		} catch (error) {
			if (current(id)) showFailure(error, 'Could not confirm removal. Refresh saved status before deciding whether to try again.');
		} finally {
			finish(id);
		}
	}

	async function reauthenticate() {
		if (!active || expired || busy !== null || !reauthRequired) return;
		const { id, signal } = begin('reauth');
		try {
			if (typeof PublicKeyCredential === 'undefined') throw new Error('passkey unavailable');
			const options = await client.reauthenticateStart(signal);
			if (!current(id)) return;
			const assertion = await navigator.credentials.get({
				publicKey: toRequestOptions({ challenge: options.challenge, rpId: options.rp_id, allowCredentials: [], timeout: 30_000 }),
				signal
			});
			if (!current(id)) return;
			if (!(assertion instanceof PublicKeyCredential)) throw new Error('no assertion');
			const serialized = serializePublicKeyCredential(assertion);
			const response = serialized.response as Record<string, unknown>;
			if (typeof serialized.id !== 'string' || typeof response.authenticatorData !== 'string' ||
				typeof response.clientDataJSON !== 'string' || typeof response.signature !== 'string') {
				throw new Error('invalid assertion');
			}
			const proof = await client.reauthenticateFinish({
				challenge: options.challenge,
				credentialId: serialized.id,
				authenticatorData: response.authenticatorData,
				clientDataJson: response.clientDataJSON,
				signature: response.signature
			}, signal);
			if (!current(id)) return;
			if (proof?.ok !== true) throw new Error('invalid proof response');
			reauthRequired = false;
			// Proof is not consent: never replay DELETE after the ceremony.
			notice = 'Identity verified. Choose your action again; removal still requires confirmation.';
		} catch (error) {
			if (!current(id)) return;
			const name = (error as { name?: unknown } | null)?.name;
			if (name === 'NotAllowedError' || name === 'AbortError') {
				errorMessage = 'The passkey step was dismissed, timed out or unavailable. No removal was attempted. You can try again or sign in again.';
			} else {
				showFailure(error, 'Could not verify your identity. No removal was attempted. Try your passkey again or sign in again.');
			}
		} finally {
			finish(id);
		}
	}
</script>

<svelte:head>
	<title>Saved provider — NNTune v2</title>
	<meta name="description" content="Check your saved provider, test reachability or remove saved settings without re-entering secrets." />
</svelte:head>

<main>
	<h1>Saved provider</h1>
	<p class="intro">Manage the provider and optional indexer already saved to your account. Passwords and API keys are never shown here.</p>

	<section class="summary" aria-label="Saved configuration" aria-busy={busy !== null}>
		<div role="status" aria-live="polite" aria-atomic="true">
			{#if expired}
				<p>Your session has expired or changed. <a href="/login">Sign in again</a> to check saved settings.</p>
			{:else if busy === 'loading'}
				<p>Loading saved-provider status…</p>
			{:else if summary}
				<p class="summary-title">{summary.provider_configured ? 'Saved provider configured' : 'No saved provider is configured'}</p>
				<p class="muted">{summary.indexer_configured ? 'Indexer settings are saved.' : 'No indexer settings are saved.'}</p>
			{:else}
				<p>Saved-provider status could not be confirmed.</p>
			{/if}
			{#if busy === 'testing'}<p>Testing saved-provider reachability…</p>{/if}
			{#if busy === 'removing'}<p>Removing saved settings…</p>{/if}
			{#if busy === 'reauth'}<p>Waiting for passkey verification…</p>{/if}
			{#if testMessage}<p>{testMessage}</p>{/if}
			{#if notice}<p>{notice}</p>{/if}
		</div>
		{#if errorMessage}
			<p role="alert">{errorMessage}</p>
		{/if}
		{#if !expired}
			<div class="actions">
				{#if summary?.provider_configured}
					<button type="button" class="nt-button" onclick={testSavedProvider} disabled={busy !== null}>Test saved provider</button>
				{/if}
				<button type="button" class="nt-button nt-button--ghost" onclick={loadSummary} disabled={busy !== null}>Refresh saved status</button>
			</div>
		{/if}
	</section>

	<p class="muted">A saved configuration is separate from a working connection. This reachability test does not verify provider sign-in or authentication.</p>

	{#if reauthRequired && !expired}
		<section aria-label="Verify your identity">
			<h2>Verify your identity</h2>
			<p>Use your account passkey, not your provider password. Verification will not repeat the removal request.</p>
			<div class="actions">
				<button type="button" class="nt-button" onclick={reauthenticate} disabled={busy !== null}>Reauthenticate with passkey</button>
				<a href="/login">Sign in again</a>
			</div>
		</section>
	{/if}

	<section class="removal" aria-labelledby="removal-heading">
		<h2 id="removal-heading">Remove saved settings</h2>
		<p>Removal clears the stored provider and indexer settings from your vault. Existing connections may continue until they end.</p>
		{#if hasSettings && !expired}
			<button type="button" class="nt-button nt-button--ghost danger" onclick={removeSavedProvider} disabled={busy !== null}>Remove saved provider</button>
		{/if}
	</section>
</main>

<style>
	main { max-width: 44rem; }
	h1 { margin-bottom: var(--nt-space-2); line-height: 1.2; }
	h2 { font-size: 1.125rem; margin-top: 0; }
	.intro { margin-bottom: var(--nt-space-4); }
	.summary { background: var(--nt-bg-raised); border: 1px solid var(--nt-border); border-radius: var(--nt-radius); padding: var(--nt-space-4); }
	.summary-title { font-weight: 600; margin-top: 0; }
	.muted { color: var(--nt-fg-muted); }
	.actions { display: flex; flex-wrap: wrap; align-items: center; gap: var(--nt-space-3); }
	button { font: inherit; cursor: pointer; border: 1px solid transparent; min-height: 44px; }
	button:disabled { opacity: 0.65; cursor: wait; }
	[role='alert'] { color: var(--nt-bad); }
	.removal { border-top: 1px solid var(--nt-border); margin-top: var(--nt-space-5); padding-top: var(--nt-space-4); }
	.danger { color: var(--nt-bad); border-color: var(--nt-bad); }
</style>
