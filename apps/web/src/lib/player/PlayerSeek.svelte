<script lang="ts">
	import { untrack } from 'svelte';
	import { formatDuration } from '../format.js';
	import type { AbsoluteSeekState } from './absoluteSeek.js';

	let { state, seek }: { state: AbsoluteSeekState; seek: (attachment: number, seconds: number, replay: boolean) => void } = $props();
	// The keyed child keeps its original receipt even before DOM teardown flushes.
	const attachment = untrack(() => state.attachment);
	const elapsed = $derived(formatDuration(state.elapsed));
	let pointerReplay: boolean | undefined;
	function endPointer(): void { pointerReplay = undefined; }
	const total = $derived(state.duration ? formatDuration(state.duration) : null);
	const value = $derived(state.duration ? state.position : 0);

	function input(event: Event & { currentTarget: HTMLInputElement }): void {
		const control = event.currentTarget;
		const requested = control.valueAsNumber;
		// Browser input is a request, not evidence. Restore the observed value
		// even if refusal leaves the owner's snapshot unchanged.
		control.valueAsNumber = value;
		seek(attachment, requested, pointerReplay ?? true);
		if (pointerReplay !== undefined) pointerReplay = false;
	}

	function keydown(event: KeyboardEvent): void {
		if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey || !state.enabled) return;
		let requested: number;
		switch (event.key) {
			case 'ArrowLeft': case 'ArrowDown': requested = value - 1; break;
			case 'ArrowRight': case 'ArrowUp': requested = value + 1; break;
			case 'PageDown': requested = value - state.duration / 10; break;
			case 'PageUp': requested = value + state.duration / 10; break;
			case 'Home': requested = 0; break;
			case 'End': requested = state.duration; break;
			default: return;
		}
		event.preventDefault();
		seek(attachment, requested, !event.repeat);
	}
</script>
<svelte:window onpointerup={endPointer} onpointercancel={endPointer} onblur={endPointer} />

<div class="nt-player-seek">
	<span aria-hidden="true">{elapsed}</span>
	<input type="range" data-testid="player-seek" aria-label="Seek"
		aria-valuetext={`${state.seeking ? `Seeking to ${formatDuration(value)}` : elapsed} of ${total ?? 'unknown duration'}`}
		min="0" max={state.duration || 1} step="any" {value} disabled={!state.enabled}
		onpointerdown={() => { pointerReplay = true; }} oninput={input} onkeydown={keydown} />
	<span aria-hidden="true">{total ?? '—'}</span>
</div>
<p class="nt-seek-feedback" aria-live="polite" aria-atomic="true">
	{#if state.seeking}<span aria-hidden="true">Seeking… </span>{/if}
	{#key state.feedbackVersion}{#if state.feedback}<span>{state.feedback}</span>{/if}{/key}
</p>

<style>
	.nt-player-seek {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: center;
		gap: var(--nt-space-2);
		font-variant-numeric: tabular-nums;
	}
	.nt-seek-feedback:empty { margin: 0; }
	input {
		width: 100%;
		min-height: 2.75rem;
		margin: 0;
		accent-color: var(--nt-accent);
	}
	input:focus-visible {
		outline: var(--nt-focus-ring-width) solid var(--nt-focus-ring);
		outline-offset: var(--nt-focus-ring-offset);
	}
</style>
