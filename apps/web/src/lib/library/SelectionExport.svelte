<script lang="ts">
	import { onDestroy } from 'svelte';
	import { createQueue, formatQueueM3U, formatQueueText } from '#lib/player/queue.js';
	import type { LibraryItem } from './cache.js';
	import { appendSelection } from './selection.js';

	let { items, selectedIds }: {
		items: readonly LibraryItem[];
		selectedIds: readonly string[];
	} = $props();

	// Resolve against all loaded rows, not the ordered/filtered view. The
	// existing append resolver supplies metadata in checkbox activation order;
	// this isolated queue is never installed in the player.
	const snapshot = $derived(appendSelection(createQueue([]), selectedIds, items).queue);
	// A refresh can replace loaded objects without changing any export data.
	// Keep feedback and pending copies in that case, but invalidate changed
	// metadata, membership or activation order (including same-tick changes).
	const selectionKey = $derived(JSON.stringify(snapshot.tracks));
	let status = $state('');
	let fallback = $state<string | null>(null);
	let generation = 0;
	let alive = true;
	let downloadUrl: string | null = null;

	function releaseDownload(): void {
		if (downloadUrl !== null) URL.revokeObjectURL(downloadUrl);
		downloadUrl = null;
	}

	function beginExport(): number {
		generation += 1;
		status = '';
		fallback = null;
		return generation;
	}

	$effect(() => {
		selectionKey;
		beginExport();
	});

	onDestroy(() => {
		alive = false;
		generation += 1;
		releaseDownload();
	});

	async function copySelectedText(): Promise<void> {
		const operation = beginExport();
		const selected = snapshot;
		const key = selectionKey;
		if (!alive || selected.tracks.length === 0) return;
		let text: string;
		try {
			// Text formatting alone does not validate ids. Apply the single
			// export-safety guard before either output path can produce bytes.
			formatQueueM3U(selected);
			text = formatQueueText(selected);
		} catch {
			status = 'Could not export the selection: it contains an unsafe track id.';
			return;
		}
		try {
			if (typeof navigator.clipboard?.writeText !== 'function') throw new Error('No clipboard');
			await navigator.clipboard.writeText(text);
		} catch {
			if (!alive || key !== selectionKey || operation !== generation) return;
			status = 'Clipboard unavailable. Copy the selected tracks manually below.';
			fallback = text;
			return;
		}
		// Selection/snapshot changes and teardown invalidate late settlements,
		// even before the reactive feedback reset has flushed.
		if (!alive || key !== selectionKey || operation !== generation) return;
		status = 'Copied selected tracks as text.';
	}

	function downloadSelectedM3U(): void {
		beginExport();
		if (!alive || snapshot.tracks.length === 0) return;
		let m3u: string;
		try {
			m3u = formatQueueM3U(snapshot);
		} catch {
			status = 'Could not export the selection: it contains an unsafe track id.';
			return;
		}
		const link = document.createElement('a');
		try {
			// Keep one URL until the next download or teardown, not a timer
			// that could revoke it while the browser is still saving the file.
			releaseDownload();
			downloadUrl = URL.createObjectURL(new Blob([m3u], { type: 'audio/x-mpegurl;charset=utf-8' }));
			link.href = downloadUrl;
			link.download = 'selected.m3u';
			link.hidden = true;
			document.body.append(link);
			link.click();
			status = 'Download started. Check your browser’s downloads for selected.m3u.';
		} catch {
			releaseDownload();
			status = 'Could not start the download. Try again or copy the selected tracks as text.';
		} finally {
			link.remove();
		}
	}
</script>

<button
	class="nt-button"
	type="button"
	data-testid="selection-export-download"
	disabled={snapshot.tracks.length === 0}
	onclick={downloadSelectedM3U}
>
	Download selected (.m3u)
</button>
<button
	class="nt-button"
	type="button"
	data-testid="selection-export-copy"
	disabled={snapshot.tracks.length === 0}
	onclick={copySelectedText}
>
	Copy selected as text
</button>
<p data-testid="selection-export-status" aria-live="polite">{status}</p>
{#if fallback !== null}
	<div class="nt-selection-export-fallback">
		<label for="selection-export-text">Selected tracks as text</label>
		<textarea
			id="selection-export-text"
			data-testid="selection-export-fallback"
			aria-describedby="selection-export-help"
			readonly
			value={fallback}
			rows="5"
		></textarea>
		<p id="selection-export-help" data-testid="selection-export-instructions">
			Select the text above, then press Ctrl+C (Command+C on Mac) or use your device’s Copy command.
		</p>
	</div>
{/if}

<style>
	button {
		min-width: 24px;
		min-height: 2.75rem;
		max-width: 100%;
		flex-shrink: 0;
	}
	[data-testid='selection-export-status'],
	.nt-selection-export-fallback {
		flex-basis: 100%;
		min-width: 0;
		margin: 0;
	}
	textarea {
		display: block;
		box-sizing: border-box;
		width: 100%;
		min-height: 24px;
	}
</style>
