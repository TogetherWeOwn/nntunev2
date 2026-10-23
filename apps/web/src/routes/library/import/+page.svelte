<script lang="ts">
	import { MAX_NZB_IMPORT_BYTES, validateImportFile } from '#lib/library/import-validation.js';

	let status: string | null = $state(null);
	let error: string | null = $state(null);
	let fileInput: HTMLInputElement | undefined = $state(undefined);

	const sizeCeilingMib = MAX_NZB_IMPORT_BYTES / 1024 / 1024;

	function formatSize(byteCount: number): string {
		if (byteCount >= 1024 * 1024) return `${(byteCount / 1024 / 1024).toFixed(2)} MiB`;
		if (byteCount >= 1024) return `${(byteCount / 1024).toFixed(1)} KiB`;
		return `${byteCount} bytes`;
	}

	function onFile(event: Event) {
		const input = event.currentTarget as HTMLInputElement;
		status = null;
		error = null;
		const file = input.files?.[0];
		if (!file) return;
		error = validateImportFile(file);
		if (error) return;
		status =
			`${file.name} (${formatSize(file.size)}) selected locally — import is not available yet. This page does not upload or process the file or add tracks to your library.`;
	}

	function onClear() {
		if (fileInput) fileInput.value = '';
		status = null;
		error = null;
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
	<p id="import-requirements">
		Choose a file with the .nzb extension that is not empty and is {sizeCeilingMib} MiB or less.
	</p>
	<label>
		NZB file
		<input
			bind:this={fileInput}
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
	{#if error || status}
		<button type="button" onclick={onClear}>Clear selection</button>
	{/if}
</main>
