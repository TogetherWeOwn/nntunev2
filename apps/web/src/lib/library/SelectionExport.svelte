<script lang="ts">
	import { onDestroy } from 'svelte';
	import { createM3UDownload, m3uDownloadFailure } from '#lib/player/m3uDownload.js';
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
	let actionKey: string | undefined;
	let downloadKey: string | undefined;
	const download = createM3UDownload();

	function beginExport(): number {
		generation += 1;
		actionKey = selectionKey;
		status = '';
		fallback = null;
		return generation;
	}

	$effect(() => {
		if (selectionKey !== actionKey) beginExport();
		if (downloadKey !== undefined && selectionKey !== downloadKey) {
			download.release();
			downloadKey = undefined;
		}
	});

	onDestroy(() => {
		alive = false;
		generation += 1;
		download.release();
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
		const key = selectionKey;
		let m3u: string;
		try {
			m3u = formatQueueM3U(snapshot);
		} catch {
			status = 'Could not export the selection: it contains an unsafe track id.';
			return;
		}
		let link: HTMLAnchorElement | undefined;
		try {
			link = document.createElement('a');
			status = download.download(link, () => m3u, 'selected.m3u');
			downloadKey = key;
		} catch {
			download.release();
			downloadKey = undefined;
			status = m3uDownloadFailure('selected tracks');
		} finally {
			link?.remove();
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
