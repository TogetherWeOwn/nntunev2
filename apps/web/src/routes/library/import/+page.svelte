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
		status =
			'NZB file selected locally — import is not available yet. This page does not upload or process the file or add tracks to your library.';
	}
</script>

<svelte:head>
	<title>NNTune — local NZB selection</title>
	<meta
		name="description"
		content="Select an NZB file locally only. No upload, processing or tracks added."
	/>
</svelte:head>

<main class="nt-wrap">
	<h1>Import</h1>
	<p>
		NZB import is not available yet. You can choose an NZB file to check its name and size —
		selection stays local only and this page does not upload or process the file or add tracks to
		your library.
	</p>
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
