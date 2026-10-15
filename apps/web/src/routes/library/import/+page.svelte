<script lang="ts">
	// NZB import shell (TOG-25176). The hooks below are the e2e contract
	// (e2e/helpers/contract.ts TESTIDS); ingest itself lands with the
	// ingest slice. Until then selecting a file explains that honestly
	// instead of faking a completed import.
	let status: string | null = $state(null);

	function onFile(event: Event) {
		const input = event.currentTarget as HTMLInputElement;
		status =
			input.files && input.files.length > 0
				? 'NZB ingest lands with the ingest slice — file received, not yet processed.'
				: null;
	}
</script>

<svelte:head>
	<title>NNTune — import</title>
	<meta name="description" content="Import an NZB into your NNTune library." />
</svelte:head>

<main class="nt-wrap">
	<h1>Import</h1>
	<p>Choose an NZB file to add its tracks to your library.</p>
	<label>
		NZB file
		<input data-testid="import-file" type="file" accept=".nzb" onchange={onFile} />
	</label>
	{#if status}
		<p data-testid="import-status" role="status">{status}</p>
	{/if}
</main>
