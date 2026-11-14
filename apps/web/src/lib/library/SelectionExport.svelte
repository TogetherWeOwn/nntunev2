<script lang="ts">
	import { onDestroy } from 'svelte';
	import { createQueue, formatQueueM3U, formatQueueText } from '#lib/player/queue.js';
	import { createTrackListExport } from '#lib/player/trackListExport.js';
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
	let alive = true;
	const exporter = createTrackListExport({
		getQueue: () => snapshot,
		getKey: () => selectionKey,
		isActive: () => alive,
		onOutcome: (text, manual) => { status = text; fallback = manual; },
		formatText: formatQueueText, formatM3U: formatQueueM3U,
		noun: 'selected tracks', filename: 'selected.m3u',
		unsafeMessage: 'Could not export the selection: it contains an unsafe track id.'
	});
	const copySelectedText = exporter.copy;
	const downloadSelectedM3U = exporter.download;

	$effect(() => {
		selectionKey;
		exporter.reset();
	});

	onDestroy(() => {
		alive = false;
		exporter.reset(true);
	});
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
