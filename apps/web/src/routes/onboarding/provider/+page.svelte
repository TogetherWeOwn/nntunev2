<script lang="ts">
	import { PUBLIC_API_BASE_URL } from '$app/env/public';
	import { ApiClient, ApiError, type ProviderTestResult } from '#lib/api/client.js';
	import { validateIndexerUrl, validateProviderConfig } from '#lib/vault-validation.js';

	const client = new ApiClient({ baseUrl: PUBLIC_API_BASE_URL });

	let step = $state<1 | 2 | 3>(1);

	// Step 1: provider.
	let host = $state('');
	let port = $state(563);
	let tls = $state(true);
	let username = $state('');
	let password = $state('');
	let maxConnections = $state(4);

	// Step 2: optional indexer.
	let indexerUrl = $state('');
	let indexerApiKey = $state('');

	let errors = $state<string[]>([]);
	let testState = $state<'idle' | 'testing' | 'ok' | 'failed'>('idle');
	let testDetail = $state('');
	let saveState = $state<'idle' | 'saving' | 'saved' | 'failed'>('idle');

	function providerInput() {
		return { host, port, tls, username, password, maxConnections };
	}

	function nextFromProvider() {
		errors = validateProviderConfig(providerInput());
		if (errors.length === 0) {
			step = 2;
		}
	}

	function nextFromIndexer() {
		const problem = validateIndexerUrl(indexerUrl);
		errors = problem === null ? [] : [problem];
		if (errors.length === 0) {
			step = 3;
		}
	}

	async function runTest() {
		errors = validateProviderConfig(providerInput());
		const indexerProblem = validateIndexerUrl(indexerUrl);
		if (indexerProblem !== null) errors = [...errors, indexerProblem];
		if (errors.length > 0) {
			testState = 'idle';
			return;
		}
		testState = 'testing';
		testDetail = '';
		try {
			const result: ProviderTestResult = await client.testProviderConnection({
				...providerInput(),
				...(indexerUrl.trim().length > 0
					? { indexerUrl: indexerUrl.trim(), indexerApiKey }
					: {})
			});
			// The response is a redacted summary: never echo credentials.
			if (result.ok) {
				testState = 'ok';
				testDetail =
					result.latencyMs !== undefined
						? `Connected in ${result.latencyMs} ms.`
						: 'Connected.';
			} else {
				testState = 'failed';
				testDetail = 'The server could not reach that provider.';
			}
		} catch (error) {
			testState = 'failed';
			testDetail =
				error instanceof ApiError && error.status === 404
					? 'The API is not deployed yet — try again later.'
					: 'Connection test failed. Check the details and try again.';
		}
	}

	async function save() {
		if (testState !== 'ok' || saveState === 'saving') return;
		saveState = 'saving';
		try {
			await client.saveProviderConfig({
				...providerInput(),
				...(indexerUrl.trim().length > 0
					? { indexerUrl: indexerUrl.trim(), indexerApiKey }
					: {})
			});
			saveState = 'saved';
		} catch {
			saveState = 'failed';
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
	<p>Step {step} of 3: {step === 1 ? 'Provider' : step === 2 ? 'Indexer (optional)' : 'Test'}</p>

	{#if errors.length > 0}
		<ul role="alert">
			{#each errors as problem}
				<li>{problem}</li>
			{/each}
		</ul>
	{/if}

	{#if step === 1}
		<form
			onsubmit={(event) => {
				event.preventDefault();
				nextFromProvider();
			}}
		>
			<label>Hostname <input name="host" required autocomplete="off" bind:value={host} /></label>
			<label>Port <input name="port" type="number" min="1" max="65535" bind:value={port} /></label>
			<label><input name="tls" type="checkbox" bind:checked={tls} /> Use TLS</label>
			<label>Username <input name="username" required autocomplete="username" bind:value={username} /></label>
			<label>
				Password
				<input name="password" type="password" required autocomplete="current-password" bind:value={password} />
			</label>
			<label>
				Connections
				<input name="maxConnections" type="number" min="1" max="100" bind:value={maxConnections} />
			</label>
			<button class="nt-button" type="submit">Continue</button>
		</form>
	{:else if step === 2}
		<form
			onsubmit={(event) => {
				event.preventDefault();
				nextFromIndexer();
			}}
		>
			<p>An indexer improves search. You can skip this and add one later.</p>
			<label>Indexer URL <input name="indexerUrl" type="url" autocomplete="off" bind:value={indexerUrl} /></label>
			<label>
				Indexer API key
				<input name="indexerApiKey" type="password" autocomplete="off" bind:value={indexerApiKey} />
			</label>
			<button class="nt-button" type="button" onclick={() => (step = 1)}>Back</button>
			<button class="nt-button" type="submit">Continue</button>
		</form>
	{:else}
		<section aria-labelledby="test-heading">
			<h2 id="test-heading">Test the connection</h2>
			<p>
				We test <strong>{host || 'your provider'}</strong>
				{indexerUrl.trim().length > 0 ? 'and your indexer ' : ''}without storing anything yet.
			</p>
			<button class="nt-button nt-button--ghost" type="button" onclick={() => (step = 2)}>
				Back
			</button>
			<button
				class="nt-button"
				type="button"
				onclick={runTest}
				disabled={testState === 'testing'}
			>
				{testState === 'testing' ? 'Testing…' : 'Test connection'}
			</button>
			{#if testState === 'ok'}
				<p role="status">{testDetail} No secrets were returned by the test.</p>
				<button
					class="nt-button"
					type="button"
					onclick={save}
					disabled={saveState === 'saving' || saveState === 'saved'}
				>
					{saveState === 'saved'
						? 'Saved'
						: saveState === 'saving'
							? 'Saving…'
							: 'Save provider'}
				</button>
			{:else if testState === 'failed'}
				<p role="alert">{testDetail}</p>
			{/if}
			{#if saveState === 'saved'}
				<p role="status">Saved. Your credentials are sealed in the encrypted vault.</p>
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
