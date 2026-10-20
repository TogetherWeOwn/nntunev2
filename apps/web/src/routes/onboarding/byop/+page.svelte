<script lang="ts">
	import { onDestroy, onMount, tick } from 'svelte';
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { readSignedInEmail } from '#lib/signed-in-email.js';
	import { serializePublicKeyCredential, toRequestOptions } from '#lib/webauthn.js';
	import {
		ApiClient,
		ApiError,
		toProviderTestInput,
		type ProviderTestResult
	} from '#lib/api/client.js';
	import {
		isIndexerAuthExpired,
		isProviderAuthExpired,
		reauthPromptFor
	} from '#lib/provider-reauth.js';
	import {
		validateIndexerFields,
		validateProviderFields,
		type IndexerFieldErrors,
		type ProviderFieldErrors
	} from '#lib/vault-validation.js';

	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });

	// Display-only identity label (see signed-in-email.ts): the account
	// these provider credentials will be saved to, shown before anything
	// is typed.
	let signedInEmail = $state<string | null>(null);
	onMount(() => {
		signedInEmail = readSignedInEmail();
	});

	let step = $state<1 | 2 | 3>(1);

	// Step 1: provider.
	let host = $state('');
	let port = $state(563);
	let tls = $state(true);
	let allowInsecurePlaintext = $state(false);
	let username = $state('');
	let password = $state('');
	let maxConnections = $state(4);

	// Step 2: optional indexer.
	let indexerUrl = $state('');
	let indexerApiKey = $state('');

	type FieldErrors = ProviderFieldErrors & IndexerFieldErrors;
	let errors = $state<FieldErrors>({});
	let stepHeading: HTMLHeadingElement;
	let testState = $state<'idle' | 'testing' | 'ok' | 'failed'>('idle');
	let testDetail = $state('');
	let reauthRequired = $state(false);
	let reauthPasswordInput = $state<HTMLInputElement | null>(null);
	let saveReauthRequired = $state(false);
	let reauthState = $state<'idle' | 'working' | 'dismissed' | 'failed'>('idle');
	let saveState = $state<'idle' | 'saving' | 'saved' | 'failed'>('idle');
	let testedConfigKey = $state<string | null>(null);
	let testRunId = 0;
	// Independent of result state: invalidating a configuration must not release
	// an operation that is still awaiting a response or a passkey ceremony.
	let busy = $state(false);
	let disposed = false;

	onDestroy(() => {
		disposed = true;
		invalidateTest();
		busy = false;
	});

	// Keep the wire payload honest: a TLS connection never carries the
	// plaintext opt-in, so clear a stale acknowledgement when TLS is on.
	$effect(() => {
		if (tls && allowInsecurePlaintext) {
			allowInsecurePlaintext = false;
		}
	});

	$effect(() => {
		if (reauthRequired) reauthPasswordInput?.focus();
	});

	function providerInput() {
		return { host, port, tls, allowInsecurePlaintext, username, password, maxConnections };
	}

	function currentTestInput() {
		return toProviderTestInput(providerInput(), indexerUrl, indexerApiKey);
	}

	function currentTestKey() {
		return JSON.stringify(currentTestInput());
	}

	function isCurrentTest(runId: number, inputKey: string) {
		return !disposed && runId === testRunId && inputKey === currentTestKey();
	}

	function invalidateTest() {
		testRunId += 1;
		testedConfigKey = null;
		testState = 'idle';
		testDetail = '';
		reauthRequired = false;
		saveReauthRequired = false;
		reauthState = 'idle';
		saveState = 'idle';
	}

	async function goTo(nextStep: 1 | 2 | 3) {
		step = nextStep;
		await tick();
		stepHeading.focus();
	}

	async function focusFirstError() {
		await tick();
		const field = Object.keys(errors)[0];
		if (field) document.getElementById(`byop-${field}`)?.focus();
	}

	function handleInput() {
		invalidateTest();
		// Only clear corrected errors after a submit; typing never moves focus
		// or starts announcing new validation failures.
		if (Object.keys(errors).length === 0) return;
		const current: FieldErrors = step === 1
			? validateProviderFields(providerInput())
			: validateIndexerFields(indexerUrl, indexerApiKey);
		for (const field of Object.keys(errors) as (keyof FieldErrors)[]) {
			if (!current[field]) delete errors[field];
		}
	}

	function backTo(previousStep: 1 | 2) {
		if (disposed || busy || saveState === 'saved') return;
		invalidateTest();
		errors = {};
		void goTo(previousStep);
	}

	function nextFromProvider() {
		errors = validateProviderFields(providerInput());
		if (Object.keys(errors).length === 0) {
			void goTo(2);
		} else {
			void focusFirstError();
		}
	}

	function nextFromIndexer() {
		errors = validateIndexerFields(indexerUrl, indexerApiKey);
		if (Object.keys(errors).length === 0) {
			void goTo(3);
		} else {
			void focusFirstError();
		}
	}

	async function runTest() {
		if (disposed || busy || saveState === 'saved') return;
		const input = currentTestInput();
		const inputKey = JSON.stringify(input);
		const providerErrors = validateProviderFields(providerInput());
		errors = { ...providerErrors, ...validateIndexerFields(indexerUrl, indexerApiKey) };
		if (Object.keys(errors).length > 0) {
			invalidateTest();
			step = Object.keys(providerErrors).length > 0 ? 1 : 2;
			await focusFirstError();
			return;
		}

		busy = true;
		const runId = ++testRunId;
		testedConfigKey = null;
		testState = 'testing';
		testDetail = '';
		reauthRequired = false;
		try {
			const result: ProviderTestResult = await client.testProviderConnection(input, () => {
				// Only an explicit Retry may repeat this UI action, and never
				// after navigation, changed input, or a newer test run.
				if (isCurrentTest(runId, inputKey) && step === 3) void runTest();
			});
			if (!isCurrentTest(runId, inputKey)) return;
			// The response is a redacted summary: never echo credentials.
			if (result.ok) {
				testedConfigKey = inputKey;
				testState = 'ok';
				testDetail =
					result.latencyMs !== undefined
						? `Provider connectivity and configuration checks passed in ${result.latencyMs} ms.`
						: 'Provider connectivity and configuration checks passed.';
			} else {
				testState = 'failed';
				testDetail = 'The server could not reach that provider.';
			}
		} catch (error) {
			if (!isCurrentTest(runId, inputKey)) return;
			if (error instanceof ApiError && error.paywall) {
				// The global paywall owns the limit response; avoid a duplicate
				// inline failure state beneath the modal.
				testState = 'idle';
				testDetail = '';
				return;
			}
			testState = 'failed';
			// Stale stored credential: inline re-auth prompt (S7), never a
			// generic error page. The step-3 prompt renders its own password
			// field (the step-1 form is unmounted by now) so the user re-enters
			// the password in place; the session stays signed in.
			if (error instanceof ApiError && isIndexerAuthExpired(error)) {
				// Wrong secret for the password prompt: name the API key and
				// point at the indexer step; never mount the password field.
				testDetail =
					reauthPromptFor(error.code) ??
					'Your indexer rejected its API key. Go back to the indexer step and re-enter it.';
			} else if (error instanceof ApiError && isProviderAuthExpired(error)) {
				reauthRequired = true;
				testDetail = reauthPromptFor(error.code) ?? 'Your provider password has expired.';
			} else {
				testDetail =
					error instanceof ApiError && error.status === 404
						? 'The API is not deployed yet — try again later.'
						: 'Connection test failed. Check the details and try again.';
			}
		} finally {
			if (testState === 'testing') testState = 'idle';
			busy = false;
		}
	}

	/**
	 * Retryable ceremony interruption: `NotAllowedError` / `AbortError`
	 * can mean dismissal, timeout, or an unavailable passkey. The name
	 * alone cannot identify the cause. Server (`ApiError`) paths never
	 * carry these names.
	 */
	function isPasskeyDismissal(error: unknown): boolean {
		const name = (error as { name?: unknown } | null)?.name;
		return name === 'NotAllowedError' || name === 'AbortError';
	}

	async function reauthenticateAndSave() {
		if (disposed || busy || saveState === 'saved' || !saveReauthRequired) return;
		const runId = testRunId;
		const inputKey = currentTestKey();
		if (testState !== 'ok' || testedConfigKey !== inputKey) return;
		if (typeof PublicKeyCredential === 'undefined') {
			reauthState = 'failed';
			return;
		}
		busy = true;
		reauthState = 'working';
		try {
			const options = await client.reauthenticateStart();
			if (!isCurrentTest(runId, inputKey)) return;
			const assertion = await navigator.credentials.get({
				publicKey: toRequestOptions({
					challenge: options.challenge,
					rpId: options.rp_id,
					allowCredentials: []
				})
			});
			if (!isCurrentTest(runId, inputKey)) return;
			if (!(assertion instanceof PublicKeyCredential)) throw new Error('no public-key credential');
			const serialized = serializePublicKeyCredential(assertion);
			const response = serialized.response as Record<string, unknown>;
			const authenticatorData = response.authenticatorData;
			const clientDataJson = response.clientDataJSON;
			const signature = response.signature;
			if (
				typeof authenticatorData !== 'string' ||
				typeof clientDataJson !== 'string' ||
				typeof signature !== 'string' ||
				typeof serialized.id !== 'string'
			) {
				throw new Error('credential missing assertion fields');
			}
			await client.reauthenticateFinish({
				challenge: options.challenge,
				credentialId: serialized.id,
				authenticatorData,
				clientDataJson,
				signature
			});
			if (!isCurrentTest(runId, inputKey) || testedConfigKey !== inputKey) return;
		} catch (error) {
			if (!isCurrentTest(runId, inputKey)) return;
			if (error instanceof ApiError && error.paywall) {
				// The global paywall owns the limit response; avoid a duplicate
				// inline failure state beneath the modal.
				reauthState = 'idle';
				return;
			}
			if (isPasskeyDismissal(error)) {
				reauthState = 'dismissed';
				return;
			}
			reauthState = 'failed';
			return;
		} finally {
			if (reauthState === 'working') reauthState = 'idle';
			busy = false;
		}

		// Transfer to save synchronously after releasing the ceremony guard.
		// Reuse the normal save path so the in-flight state, tested-input guard,
		// and paywall/error handling stay identical after reauthentication.
		await save();
	}

	async function save() {
		if (disposed || busy || saveState === 'saved') return;
		const input = currentTestInput();
		const inputKey = JSON.stringify(input);
		if (testState !== 'ok' || testedConfigKey !== inputKey) {
			if (testedConfigKey !== inputKey) invalidateTest();
			return;
		}

		busy = true;
		const runId = testRunId;
		saveReauthRequired = false;
		reauthState = 'idle';
		saveState = 'saving';
		try {
			await client.saveProviderConfig(input);
			if (isCurrentTest(runId, inputKey)) {
				saveState = 'saved';
				window.location.href = '/library';
			}
		} catch (error) {
			if (!isCurrentTest(runId, inputKey)) return;
			if (error instanceof ApiError && error.paywall) {
				saveState = 'idle';
				return;
			}
			saveReauthRequired = error instanceof ApiError && error.code === 'reauth_required';
			saveState = 'failed';
		} finally {
			if (saveState === 'saving') saveState = 'idle';
			busy = false;
		}
	}
</script>

<svelte:head>
	<title>NNTune — connect your provider</title>
	<meta
		name="description"
		content="Connect your own Usenet provider and optional indexer to NNTune."
	/>
</svelte:head>

<main class="nt-wrap">
	<h1>Connect your provider</h1>
	{#if signedInEmail !== null}
		<p role="status">
			Signed in as <strong>{signedInEmail}</strong> — the provider details you save go to
			this account. If that's not you, stop here and <a href="/login">sign in again</a>.
		</p>
	{/if}
	<p role="status" aria-atomic="true">Step {step} of 3: {step === 1 ? 'Provider' : step === 2 ? 'Indexer (optional)' : 'Test'}</p>
	<h2 id="byop-step-heading" bind:this={stepHeading} tabindex="-1">
		{step === 1 ? 'Provider details' : step === 2 ? 'Indexer (optional)' : 'Test the connection'}
	</h2>
	<p role="status" aria-atomic="true">
		{testState === 'testing' ? 'Testing the connection…' : saveState === 'saving' ? 'Saving provider details…' : reauthState === 'working' ? 'Verifying your passkey…' : ''}
	</p>

	{#if Object.keys(errors).length > 0}
		<ul role="alert">
			{#each Object.entries(errors) as [field, problem] (field)}
				<li id={`byop-${field}-error`}>
					<a href={`#byop-${field}`} onclick={(event) => {
						event.preventDefault();
						document.getElementById(`byop-${field}`)?.focus();
					}}>{problem}</a>
				</li>
			{/each}
		</ul>
	{/if}

	{#if step === 1}
		<form
			oninput={handleInput}
			onchange={handleInput}
			onsubmit={(event) => {
				event.preventDefault();
				nextFromProvider();
			}}
		>
			<label>Hostname <input id="byop-host" aria-invalid={errors.host ? 'true' : undefined} aria-describedby={errors.host ? 'byop-host-error' : undefined} data-testid="byop-host" name="host" required autocomplete="off" bind:value={host} /></label>
			<label>
				Port
				<select id="byop-port" aria-invalid={errors.port ? 'true' : undefined} aria-describedby={errors.port ? 'byop-port-error' : undefined} data-testid="byop-port" name="port" required bind:value={port}>
					<option value={119}>119 (plaintext NNTP)</option>
					<option value={563}>563 (TLS)</option>
				</select>
			</label>
			<label><input data-testid="byop-tls" name="tls" type="checkbox" bind:checked={tls} /> Use TLS</label>
			{#if !tls}
				<p role="alert">
					Without TLS your password is sent in cleartext. Only continue without TLS if
					your provider requires it.
				</p>
				<label>
					<input
						id="byop-allowInsecurePlaintext"
						aria-invalid={errors.allowInsecurePlaintext ? 'true' : undefined}
						aria-describedby={errors.allowInsecurePlaintext ? 'byop-allowInsecurePlaintext-error' : undefined}
						name="allowInsecurePlaintext"
						type="checkbox"
						bind:checked={allowInsecurePlaintext}
					/>
					I understand my password will be sent unencrypted
				</label>
			{/if}
			<label>Username <input id="byop-username" aria-invalid={errors.username ? 'true' : undefined} aria-describedby={errors.username ? 'byop-username-error' : undefined} data-testid="byop-user" name="username" required autocomplete="username" bind:value={username} /></label>
			<label>
				Password
				<input id="byop-password" aria-invalid={errors.password ? 'true' : undefined} aria-describedby={errors.password ? 'byop-password-error' : undefined} data-testid="byop-pass" name="password" type="password" required autocomplete="current-password" bind:value={password} />
			</label>
			<label>
				Connections
				<input
					id="byop-maxConnections"
					aria-invalid={errors.maxConnections ? 'true' : undefined}
					aria-describedby={errors.maxConnections ? 'byop-maxConnections-error' : undefined}
					name="maxConnections"
					type="number"
					min="1"
					max="50"
					bind:value={maxConnections}
				/>
			</label>
			<button class="nt-button" type="submit">Continue</button>
		</form>
	{:else if step === 2}
		<form
			oninput={handleInput}
			onchange={handleInput}
			onsubmit={(event) => {
				event.preventDefault();
				nextFromIndexer();
			}}
		>
			<p>An indexer improves search. You can skip this and add one later.</p>
			<label>Indexer URL <input id="byop-indexerUrl" aria-invalid={errors.indexerUrl ? 'true' : undefined} aria-describedby={errors.indexerUrl ? 'byop-indexerUrl-error' : undefined} name="indexerUrl" type="url" autocomplete="off" bind:value={indexerUrl} /></label>
			<label>
				Indexer API key
				<input id="byop-indexerApiKey" aria-invalid={errors.indexerApiKey ? 'true' : undefined} aria-describedby={errors.indexerApiKey ? 'byop-indexerApiKey-error' : undefined} name="indexerApiKey" type="password" autocomplete="off" bind:value={indexerApiKey} />
			</label>
			<button class="nt-button" type="button" onclick={() => backTo(1)}>Back</button>
			<button class="nt-button" type="submit">Continue</button>
		</form>
	{:else}
		<section aria-labelledby="byop-step-heading" aria-busy={testState === 'testing' || saveState === 'saving' || reauthState === 'working'}>
			<p>
				We check connectivity to <strong>{host || 'your provider'}</strong> and validate the configuration and addresses{indexerUrl.trim().length > 0 ? ' (including your indexer URL)' : ''}, without storing anything yet.
				This does not verify provider credentials{indexerUrl.trim().length > 0 ? ' or contact or authenticate to your indexer' : ''}.
			</p>
			<button
				class="nt-button nt-button--ghost"
				type="button"
				onclick={() => backTo(2)}
				disabled={busy || saveState === 'saved'}
			>
				Back
			</button>
			<button
				class="nt-button"
				type="button"
				onclick={runTest}
					data-testid="byop-test-connection"
				disabled={busy || saveState === 'saved'}
			>
				{testState === 'testing' ? 'Testing…' : 'Test connection'}
			</button>
			{#if testState === 'ok'}
				<p data-testid="byop-status" role="status">{testDetail} No secrets were returned by the test.</p>
				<button
					class="nt-button"
					type="button"
					onclick={save}
						data-testid="byop-save"
					disabled={busy || saveState === 'saved'}
				>
					{saveState === 'saved'
						? 'Saved'
						: saveState === 'saving'
							? 'Saving…'
							: 'Save provider'}
				</button>
			{:else if testState === 'failed'}
				{#if reauthRequired}
					<div data-testid="byop-status" role="alert" aria-labelledby="reauth-heading">
						<h3 id="reauth-heading">Provider credentials expired</h3>
						<p>{testDetail}</p>
						<p>
							Re-enter your password below and repeat the connectivity and configuration checks before saving.
							These checks do not verify your password. Your session stays signed in.
						</p>
						<label>
							Password
							<input
								name="reauthPassword"
								bind:this={reauthPasswordInput}
								type="password"
								required
								autocomplete="current-password"
								bind:value={password}
								oninput={() => {
									// A changed password voids any earlier test result,
									// but typing must not dismiss this prompt.
									testedConfigKey = null;
									saveState = 'idle';
								}}
							/>
						</label>
						<button class="nt-button" type="button" onclick={runTest} disabled={busy}>Test again</button>
					</div>
				{:else}
					<p data-testid="byop-status" role="alert">{testDetail}</p>
				{/if}
			{/if}
			{#if saveState === 'saved'}
				<p role="status">Saved. Your credentials are sealed in the encrypted vault.</p>
			{:else if saveState === 'failed' && saveReauthRequired}
				<div role="alert">
					<p>Your session is too old to save provider details. Reauthenticate with your passkey to continue.</p>
					<button
						class="nt-button"
						type="button"
						onclick={reauthenticateAndSave}
						disabled={busy}
					>
						{reauthState === 'working' ? 'Verifying…' : 'Reauthenticate and save'}
					</button>
					{#if reauthState === 'dismissed'}
						<p role="status">Passkey reauthentication didn't complete. Possible reasons: You dismissed the passkey prompt, it timed out, or no usable passkey was available. Try again when ready.</p>
					{:else if reauthState === 'failed'}
						<p role="status">Reauthentication failed. <a href="/login">Sign in again</a> to continue.</p>
					{/if}
				</div>
			{:else if saveState === 'failed'}
				<p role="alert">Could not save. Your details were not stored — try again.</p>
			{/if}
		</section>
	{/if}
</main>

<style>
	form,
	section {
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
