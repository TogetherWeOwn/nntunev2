<script lang="ts">
	import { onDestroy } from 'svelte';
	import { formatQueueM3U, formatQueueText, type QueueState } from './queue.js';

	let { queue, authGeneration, loading = false }: {
		queue: QueueState;
		authGeneration: number;
		loading?: boolean;
	} = $props();

	let status = $state('');
	let fallback = $state<string | null>(null);
	let generation = 0;
	let alive = true;
	let downloadUrl: string | null = null;

	function releaseDownload(): void {
		if (downloadUrl !== null) URL.revokeObjectURL(downloadUrl);
		downloadUrl = null;
	}

	function clearFeedback(): number {
		generation += 1;
		status = '';
		fallback = null;
		return generation;
	}

	// Queue replacements, including selection, invalidate feedback. Snapshot
	// identity also fences same-tick edits before this effect has flushed.
	$effect(() => {
		queue;
		clearFeedback();
	});

	$effect(() => {
		authGeneration;
		clearFeedback();
		releaseDownload();
	});

	onDestroy(() => {
		alive = false;
		clearFeedback();
		releaseDownload();
	});

	async function copyQueueText(): Promise<void> {
		const action = clearFeedback();
		const snapshot = queue;
		const account = authGeneration;
		if (!alive || snapshot.tracks.length === 0) return;
		const text = formatQueueText(snapshot);
		try {
			// The clipboard getter and the write may both fail; keep manual copy usable.
			if (typeof navigator.clipboard?.writeText !== 'function') throw new Error('No clipboard');
			await navigator.clipboard.writeText(text);
		} catch {
			if (!alive || queue !== snapshot || account !== authGeneration || action !== generation) return;
			status = 'Clipboard unavailable. Copy the queue manually below.';
			fallback = text;
			return;
		}
		if (!alive || queue !== snapshot || account !== authGeneration || action !== generation) return;
		status = 'Copied queue as text.';
	}

	function downloadQueueM3U(): void {
		clearFeedback();
		if (!alive || queue.tracks.length === 0) return;
		const link = document.createElement('a');
		try {
			// Keep at most one URL until the next download, auth reset or teardown.
			releaseDownload();
			const blob = new Blob([formatQueueM3U(queue)], { type: 'audio/x-mpegurl;charset=utf-8' });
			downloadUrl = URL.createObjectURL(blob);
			link.href = downloadUrl;
			link.download = 'queue.m3u';
			link.hidden = true;
			document.body.append(link);
			link.click();
			status = 'Download started. Check your browser’s downloads for queue.m3u.';
		} catch {
			releaseDownload();
			status = 'Could not start the download. Try again or copy the queue as text.';
		} finally {
			link.remove();
		}
	}
</script>

<div class="queue-export">
	<div class="queue-export-actions">
		<button
			class="nt-button"
			type="button"
			disabled={queue.tracks.length === 0}
			aria-describedby="queue-export-scope queue-export-status"
			onclick={downloadQueueM3U}
		>
			Download queue (.m3u)
		</button>
		<button
			class="nt-button"
			type="button"
			disabled={queue.tracks.length === 0}
			aria-describedby="queue-export-scope queue-export-status"
			onclick={() => void copyQueueText()}
		>
			Copy queue as text
		</button>
	</div>
	<p id="queue-export-scope">Whole queue, in arranged order. Track IDs only in M3U — no audio or stream links. This queue is not kept across reloads.</p>
	<p id="queue-export-status" data-testid="queue-export-status" aria-live={loading ? 'off' : 'polite'}>
		{queue.tracks.length === 0 ? 'Nothing to export: the queue is empty. Add a track first.' : status}
	</p>
	{#if fallback !== null}
		<label for="queue-export-fallback-field">Queue as text</label>
		<textarea
			id="queue-export-fallback-field"
			data-testid="queue-export-fallback"
			readonly
			rows="4"
			value={fallback}
			aria-describedby="queue-export-instructions"
		></textarea>
		<p id="queue-export-instructions" data-testid="queue-export-instructions">
			Select the text above and use Ctrl+C / Cmd+C, or long-press to copy manually.
		</p>
	{/if}
</div>

<style>
	.queue-export {
		margin-block: var(--nt-space-4);
	}
	.queue-export-actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--nt-space-2);
	}
	.queue-export button {
		min-width: 24px;
		min-height: 44px;
		max-width: 100%;
		flex-shrink: 0;
	}
	.queue-export p {
		font-size: 0.875rem;
	}
	.queue-export textarea {
		display: block;
		box-sizing: border-box;
		width: 100%;
		padding: var(--nt-space-2);
		font: inherit;
		color: var(--nt-fg);
		background: var(--nt-bg-raised);
		border: 1px solid var(--nt-border);
		border-radius: var(--nt-radius);
	}
</style>
