<script lang="ts">
	import { onDestroy, onMount, tick } from 'svelte';
	import { MAX_NZB_IMPORT_BYTES } from '#lib/library/import-validation.js';
	import { selectImportFiles, type ImportSelection } from '#lib/library/import-selection.js';
	import { NZBP_ERROR_TEXT } from '#lib/library/nzb-preview-copy.js';
	import { NZBP_LIMITS, nzbpError, prescanNzbProlog, summarizeNzbElements, type NzbpElement, type NzbpOutcome, type NzbpErrorCode } from '#lib/library/nzb-preview.js';

	let selection: ImportSelection = $state({ status: null, error: null });
	let fileInput: HTMLInputElement | undefined = $state(undefined);
	let dragging = $state(false);
	let selectionVersion = $state(0);
	let clearOutcome = $state('');
	let selectedFile: File | null = $state(null);
	let busy = $state(false);
	let outcome: NzbpOutcome | null = $state(null);
	let previewStatus = $state('');
	let generation = 0;
	let active: AbortController | null = null;
	let readStarted = false;
	const summary = $derived.by(() => outcome?.status === 'ok' ? outcome.summary : null);
	const previewError = $derived.by(() => outcome?.status === 'error' ? NZBP_ERROR_TEXT[outcome.error_code] : null);
	const sizeCeilingMib = MAX_NZB_IMPORT_BYTES / 1024 / 1024;
	const taskTurn = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

	function expose(value: NzbpOutcome | { status: 'cancelled' } | null) {
		// Aggregate-only inspection seam, cleared with the component lifecycle too.
		if (typeof window !== 'undefined') (window as Window & { __nzbpLastOutcome?: unknown }).__nzbpLastOutcome = value;
	}

	function invalidate() {
		generation++;
		active?.abort();
		active = null;
		outcome = null;
		busy = false;
		previewStatus = '';
		expose(null);
	}
	onMount(() => expose(null));
	onDestroy(invalidate);

	function setSelection(next: ImportSelection, file: File | null = null) {
		selection = next;
		selectedFile = next.error ? null : file;
		// Refresh the text inside the same live region even for identical outcomes.
		selectionVersion += 1;
		clearOutcome = '';
	}

	function onFile(event: Event) {
		invalidate();
		const input = event.currentTarget as HTMLInputElement;
		dragging = false;
		if (!input.files?.length) { resetSelection(); return; }
		setSelection(selectImportFiles(Array.from(input.files)), input.files.length === 1 ? input.files[0] : null);
	}

	function onCancel(event: Event) {
		invalidate();
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
		invalidate();
		event.preventDefault();
		dragging = false;
		if (fileInput) fileInput.value = '';
		const transfer = event.dataTransfer;
		const hasDirectory = Array.from(transfer?.items ?? []).some(
			(item) => item.kind === 'file' && item.webkitGetAsEntry?.()?.isDirectory
		);
		const files = hasDirectory ? [] : Array.from(transfer?.files ?? []);
		setSelection(selectImportFiles(files), files.length === 1 ? files[0] : null);
		if (fileInput && !hasDirectory && transfer?.files.length === 1) fileInput.files = transfer.files;
	}

	function resetSelection() {
		invalidate();
		if (fileInput) fileInput.value = '';
		dragging = false;
		if (!selection.error && !selection.status) return;
		setSelection({ status: null, error: null });
		clearOutcome = 'Local selection cleared.';
	}

	async function onClear() {
		resetSelection();
		const clearedVersion = selectionVersion;
		await tick();
		// Only Clear recovers focus, and never after a newer selection action.
		if (selectionVersion === clearedVersion) fileInput?.focus({ preventScroll: true });
	}

	function cancelPreview() {
		const wasReading = readStarted;
		invalidate();
		previewStatus = wasReading ? 'Preview cancelled — the read was stopped.' : 'Preview cancelled — nothing was read.';
		expose({ status: 'cancelled' });
	}

	// Adapt lazily: no recursive tree copy or retained non-numeric attribute values.
	function adapt(node: Element): NzbpElement {
		return {
			get localName() { return node.localName; },
			get namespaceURI() { return node.namespaceURI; },
			get attributes() { return node.attributes; },
			get childElements() {
				return { *[Symbol.iterator]() {
					for (let child = node.firstElementChild; child; child = child.nextElementSibling) yield adapt(child);
				} };
			},
			get textContent() { return node.textContent ?? ''; }
		};
	}

	async function readLocal(file: File, signal: AbortSignal): Promise<Uint8Array | NzbpErrorCode | null> {
		if (file.size > NZBP_LIMITS.maxBytes) return 'NZBP_TOO_LARGE';
		if (file.size === 0) return 'NZBP_EMPTY';
		if (signal.aborted) return null;
		readStarted = true;
		try {
			if (typeof file.stream !== 'function') {
				// Non-interruptible fallback: actual byte cap before decode; generation-discard only.
				const buffer = await file.arrayBuffer();
				if (signal.aborted) return null;
				if (buffer.byteLength > NZBP_LIMITS.maxBytes) return 'NZBP_TOO_LARGE';
				return buffer.byteLength === 0 ? 'NZBP_EMPTY' : new Uint8Array(buffer);
			}
			const reader = file.stream().getReader();
			const cancelRead = () => { void reader.cancel().catch(() => {}); };
			signal.addEventListener('abort', cancelRead, { once: true });
			const chunks: Uint8Array[] = [];
			let actualBytes = 0;
			try {
				for (;;) {
					if (signal.aborted) return null;
					const { done, value: chunk } = await reader.read();
					if (signal.aborted) return null;
					if (done) break;
					actualBytes += chunk.byteLength;
					if (actualBytes > NZBP_LIMITS.maxBytes) {
						await reader.cancel().catch(() => {});
						return 'NZBP_TOO_LARGE';
					}
					// Native stream chunks can exceed 64 KiB. Bound each retained chunk
					// and yield between them, not just between browser-sized reads.
					for (let offset = 0; offset < chunk.byteLength; offset += 64 * 1024) {
						chunks.push(chunk.slice(offset, offset + 64 * 1024));
						await taskTurn();
						if (signal.aborted) return null;
					}
				}
			} finally {
				signal.removeEventListener('abort', cancelRead);
				reader.releaseLock();
			}
			if (actualBytes === 0) return 'NZBP_EMPTY';
			const bytes = new Uint8Array(actualBytes);
			let offset = 0;
			for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
			return bytes;
		} catch {
			return signal.aborted ? null : 'NZBP_READ_FAILED';
		}
	}

	async function preview() {
		invalidate();
		const file = selectedFile;
		if (!file) return;
		const ownGeneration = generation;
		const snapshot = { name: file.name, size: file.size, lastModified: file.lastModified };
		const controller = new AbortController();
		active = controller;
		const current = () => generation === ownGeneration && !controller.signal.aborted && selectedFile === file &&
			file.name === snapshot.name && file.size === snapshot.size && file.lastModified === snapshot.lastModified;
		busy = true;
		readStarted = false;
		previewStatus = 'Reading the file locally…';
		await taskTurn();
		if (!current()) return;
		let result: NzbpOutcome;
		try {
			const bytes = await readLocal(file, controller.signal);
			if (!current() || bytes === null) return;
			if (typeof bytes === 'string') result = nzbpError(bytes);
			else {
				let decoded: string | null = null;
				try { decoded = new TextDecoder('utf-8', { fatal: true }).decode(bytes); } catch { /* constant outcome only */ }
				if (decoded === null) result = nzbpError('NZBP_ENCODING_UNSUPPORTED');
				else {
					const scanned = prescanNzbProlog(decoded);
					if (!scanned.ok) result = nzbpError(scanned.errorCode);
					else if (typeof DOMParser === 'undefined') result = nzbpError('NZBP_PARSER_UNAVAILABLE');
					else {
						const parsed = new DOMParser().parseFromString(scanned.documentText, 'text/xml');
						// Parsing is synchronous. Queued cancel/replace/Clear must land before
						// either traversal OR parser-error publication; never inspect error text.
						await taskTurn();
						if (!current()) return;
						result = parsed.getElementsByTagNameNS('*', 'parsererror').length || !parsed.documentElement
							? nzbpError('NZBP_MALFORMED_XML') : summarizeNzbElements(adapt(parsed.documentElement));
					}
				}
			}
		} catch {
			// No caught error messages or parser-generated excerpts ever escape.
			result = nzbpError('NZBP_MALFORMED_XML');
		}
		await taskTurn();
		if (!current()) return;
		active = null;
		busy = false;
		previewStatus = '';
		outcome = result;
		expose(result);
	}

	function declaredSize(bytes: string): string {
		// Only called for the capped, exact decimal aggregate. Conversion is display-only.
		return `${(Number(bytes) / 1024 / 1024).toFixed(1)} MiB`;
	}
</script>

<svelte:window
	ondragend={() => dragging = false}
	onblur={() => dragging = false}
	onkeydown={(event) => { if (event.key === 'Escape') { if (busy) cancelPreview(); dragging = false; } }}
/>

<svelte:head>
	<title>NNTune — local NZB selection and preview</title>
	<meta name="description" content="Local NZB selection alone never uploads, processes or adds tracks. Explicit Preview reads aggregate claims locally; import is not available yet." />
</svelte:head>

<main class="nt-wrap">
	<h1>Import</h1>
	<p>
		NZB import is not available yet. Selection checks the name and size as metadata only.
		Selection alone does not upload or process the file or add tracks to your library.
		Use the explicit Preview gesture for a local read of aggregate claims.
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
				aria-invalid={selection.error ? 'true' : undefined}
				aria-describedby={`import-requirements${selection.error || selection.status ? ' import-feedback' : ''}${outcome || busy || previewStatus ? ' import-preview' : ''}`}
				onchange={onFile}
				oncancel={onCancel}
			/>
		</label>
	</fieldset>
	{#if selection.error || selection.status}
		<p id="import-feedback" data-testid="import-status" role={selection.error || previewError ? 'alert' : 'status'} aria-atomic="true">
			{#key selectionVersion}{selection.error ?? selection.status}{/key}{#if previewStatus} {previewStatus}{/if}{#if previewError} {previewError}{/if}
		</p>
	{/if}
	<div class="actions">
		<button class="nt-button" type="button" disabled={!selectedFile} onclick={() => busy ? cancelPreview() : preview()}>{busy ? 'Cancel' : 'Preview'}</button>
		{#if selection.error || selection.status}
			<button class="nt-button nt-button--ghost" type="button" onclick={onClear}>Clear selection</button>
		{/if}
	</div>
	<div aria-live="polite" aria-atomic="true">{clearOutcome}</div>
	<div id="import-preview" role={summary ? 'status' : undefined} aria-atomic="true">
		{#if summary}
			<section aria-labelledby="preview-heading">
				<h2 id="preview-heading">Local preview of what this NZB claims</h2>
				<p>Local preview only — nothing was uploaded and nothing was checked on any server.</p>
				<ul>
					<li>This file claims {summary.file_entry_count} file entries and {summary.segment_count} segments.</li>
					{#if summary.declared_bytes_total !== null}
						<li>Declared size: {declaredSize(summary.declared_bytes_total)} — the sum of the size claims written inside this file.</li>
					{:else if summary.declared_bytes_basis === 'no_segments'}
						<li>Declared size: unknown — this file claims no segments.</li>
					{:else}
						<li>Declared size: unknown — {summary.segments_without_byte_claim} {summary.segments_without_byte_claim === 1 ? 'segment does' : 'segments do'} not state a size, so no total is shown.</li>
					{/if}
					{#if summary.files_without_segments > 0}<li>{summary.files_without_segments} file {summary.files_without_segments === 1 ? 'entry claims' : 'entries claim'} no segments.</li>{/if}
					{#if summary.duplicate_segment_numbers > 0}<li>{summary.duplicate_segment_numbers} {summary.duplicate_segment_numbers === 1 ? 'segment repeats' : 'segments repeat'} a segment number inside their file entry.</li>{/if}
					{#if summary.duplicate_message_ids > 0}<li>{summary.duplicate_message_ids} {summary.duplicate_message_ids === 1 ? 'segment repeats' : 'segments repeat'} another segment's message identifier.</li>{/if}
					{#if summary.segments_without_number > 0}<li>{summary.segments_without_number} {summary.segments_without_number === 1 ? 'segment does' : 'segments do'} not state a segment number.</li>{/if}
					{#if summary.files_with_multiple_segment_groups > 0}<li>{summary.files_with_multiple_segment_groups} file {summary.files_with_multiple_segment_groups === 1 ? 'entry claims' : 'entries claim'} multiple segment groups.</li>{/if}
				</ul>
				<p>A readable NZB does not mean the content exists, is safe, belongs to you, or will play. Names inside an NZB are written by whoever posted it and are not shown here.</p>
				<p>Remember: a readable preview does not mean a future import would accept this file.</p>
			</section>
		{/if}
	</div>
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
	fieldset.dragging { border-color: var(--nt-accent); }
	legend { font-weight: 600; padding-inline: var(--nt-space-2); }
	label { display: grid; gap: var(--nt-space-2); }
	input { max-width: 100%; }
	#import-feedback, #import-preview { overflow-wrap: anywhere; }
	[role='alert'] { color: var(--nt-bad); }
	.actions { display: flex; gap: var(--nt-space-3); }
	button { min-height: 44px; font: inherit; cursor: pointer; }
	@media (max-width: 480px) { .actions { flex-direction: column; } }
</style>
