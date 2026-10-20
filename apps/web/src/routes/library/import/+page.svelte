<script lang="ts">
	import { validateImportFile } from '#lib/library/import-validation.js';

	let status: string | null = $state(null);
	let error: string | null = $state(null);

	function onFile(event: Event) {
		const input = event.currentTarget as HTMLInputElement;
		status = null;
		error = null;
		const file = input.files?.[0];
		if (!file) return;
		error = validateImportFile(file);
		if (error) return;
		status = 'NZB import is not available yet.';
	}
</script>

<svelte:head>
	<title>NNTune — import</title>
	<meta
		name="description"
		content="Check an NZB file's name and size. NZB import is not available yet."
	/>
</svelte:head>

<main class="nt-wrap">
	<h1>Import</h1>
	<p>NZB import is not available yet. You can check an NZB file's name and size.</p>
	<label>
		NZB file
		<input
			data-testid="import-file"
			type="file"
			accept=".nzb"
			aria-describedby={error || status ? 'import-feedback' : undefined}
			onchange={onFile}
		/>
	</label>
	{#if error}
		<p id="import-feedback" data-testid="import-status" role="alert">{error}</p>
	{:else if status}
		<p id="import-feedback" data-testid="import-status" role="status">{status}</p>
	{/if}
</main>
