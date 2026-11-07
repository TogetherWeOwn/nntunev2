<script lang="ts">
	import ExportFallback from './ExportFallback.svelte';
	import { usePlayerSession } from './context.js';

	const player = usePlayerSession();
	const exporter = player.queueExport;
</script>

<div class="queue-export">
	<div class="queue-export-actions">
		<button
			class="nt-button"
			type="button"
			disabled={$player.queue.tracks.length === 0}
			aria-describedby="queue-export-scope queue-export-status"
			onclick={exporter.download}
		>
			Download queue (.m3u)
		</button>
		<button
			class="nt-button"
			type="button"
			disabled={$player.queue.tracks.length === 0}
			aria-describedby="queue-export-scope queue-export-status"
			onclick={() => void exporter.copy()}
		>
			Copy queue as text
		</button>
	</div>
	<p id="queue-export-scope">Whole queue, in arranged order. Track IDs only in M3U — no audio or stream links. This queue is not kept across reloads.</p>
	<p id="queue-export-status" data-testid="queue-export-status" aria-live={$player.queueLiveRegions ? 'polite' : 'off'}>
		{$player.queue.tracks.length === 0 ? 'Nothing to export: the queue is empty. Add a track first.' : $exporter.status}
	</p>
	{#if $exporter.fallback !== null}
		<ExportFallback
			prefix="queue-export"
			fieldId="queue-export-fallback-field"
			label="Queue as text"
			value={$exporter.fallback}
			instructions="Select the text above and use Ctrl+C / Cmd+C, or long-press to copy manually."
		/>
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
	.queue-export :global(p) {
		font-size: 0.875rem;
	}
	.queue-export :global(textarea) {
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
