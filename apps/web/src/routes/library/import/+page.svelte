<script lang="ts">
	import { MAX_NZB_IMPORT_BYTES } from '#lib/library/import-validation.js';
	import { selectImportFiles, type ImportSelection } from '#lib/library/import-selection.js';

	let selection: ImportSelection = $state({ status: null, error: null });
	let fileInput: HTMLInputElement | undefined = $state(undefined);
	let dragging = $state(false);

	const sizeCeilingMib = MAX_NZB_IMPORT_BYTES / 1024 / 1024;

	function onFile(event: Event) {
		const input = event.currentTarget as HTMLInputElement;
		dragging = false;
		if (!input.files?.length) {
			onClear();
			return;
		}
		selection = selectImportFiles(Array.from(input.files));
	}

	function onCancel(event: Event) {
		dragging = false;
		// A native chooser also fires cancel when the same file is selected again.
		if ((event.currentTarget as HTMLInputElement).files?.length) onFile(event);
	}

	function onDragOver(event: DragEvent) {
		event.preventDefault();
		if (event.dataTransfer) event.dataTransfer.dropEffect = 'copy';
		dragging = true;
	}

	function onDragLeave(event: DragEvent) {
		const area = event.currentTarget as HTMLFieldSetElement;
		if (event.relatedTarget instanceof Node && area.contains(event.relatedTarget)) return;
		dragging = false;
	}

	function onDrop(event: DragEvent) {
		event.preventDefault();
		dragging = false;
		if (fileInput) fileInput.value = '';
		const transfer = event.dataTransfer;
		const hasDirectory = Array.from(transfer?.items ?? []).some(
			(item) => item.kind === 'file' && item.webkitGetAsEntry?.()?.isDirectory
		);
		selection = selectImportFiles(hasDirectory ? [] : Array.from(transfer?.files ?? []));
		if (fileInput && !hasDirectory && transfer?.files.length === 1) {
			fileInput.files = transfer.files;
		}
	}

	function onClear() {
		if (fileInput) fileInput.value = '';
		dragging = false;
		selection = { status: null, error: null };
	}
</script>

<svelte:window
	ondragend={() => dragging = false}
	onblur={() => dragging = false}
	onkeydown={(event) => { if (event.key === 'Escape') dragging = false; }}
/>

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
	<fieldset
		data-testid="import-drop-area"
		class:dragging
		ondragenter={onDragOver}
		ondragover={onDragOver}
		ondragleave={onDragLeave}
		ondrop={onDrop}
	>
		<legend>Drop one local NZB file</legend>
		<p id="import-requirements">
			Choose a file with the .nzb extension that is not empty and is {sizeCeilingMib} MiB or less.
			Or use the file picker below.
		</p>
		<label>
			NZB file
			<input
				bind:this={fileInput}
				data-testid="import-file"
				type="file"
				accept=".nzb"
				aria-describedby={selection.error || selection.status ? 'import-requirements import-feedback' : 'import-requirements'}
				onchange={onFile}
				oncancel={onCancel}
			/>
		</label>
	</fieldset>
	{#if selection.error}
		<p id="import-feedback" data-testid="import-status" role="alert">{selection.error}</p>
	{:else if selection.status}
		<p id="import-feedback" data-testid="import-status" role="status">{selection.status}</p>
	{/if}
	{#if selection.error || selection.status}
		<button class="nt-button nt-button--ghost" type="button" onclick={onClear}>Clear selection</button>
	{/if}
</main>

<style>
	fieldset {
		min-width: 0;
		margin: var(--nt-space-4) 0;
		padding: var(--nt-space-3) var(--nt-space-4) var(--nt-space-4);
		border: 2px dashed var(--nt-border);
		border-radius: var(--nt-radius);
		background: var(--nt-bg-raised);
	}

	fieldset.dragging {
		border-color: var(--nt-accent);
	}

	legend {
		font-weight: 600;
		padding-inline: var(--nt-space-2);
	}

	label {
		display: grid;
		gap: var(--nt-space-2);
	}

	input {
		max-width: 100%;
	}

	#import-feedback {
		overflow-wrap: anywhere;
	}

	[role='alert'] {
		color: var(--nt-bad);
	}

	button {
		font: inherit;
		cursor: pointer;
	}
</style>
