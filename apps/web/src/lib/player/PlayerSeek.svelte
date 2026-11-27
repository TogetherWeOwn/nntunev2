<script lang="ts">
	import { untrack } from 'svelte';
	import { formatDuration } from '../format.js';
	import type { AbsoluteSeekState } from './absoluteSeek.js';

	let { state, seek }: { state: AbsoluteSeekState; seek: (attachment: number, seconds: number, replay: boolean) => void } = $props();
	// The keyed child keeps its original receipt even before DOM teardown flushes.
	const attachment = untrack(() => state.attachment);
	const elapsed = $derived(formatDuration(state.elapsed));
	let pointer: { id: number; replay: boolean } | undefined;
	function endPointer(event: PointerEvent): void {
		if (event.pointerId === pointer?.id) pointer = undefined;
	}
	const duration = $derived(Number.isFinite(state.duration) && state.duration > 0 ? state.duration : 0);
	const validPosition = $derived(Number.isFinite(state.position) && state.position >= 0 && state.position <= duration);
	const value = $derived(validPosition ? state.position : 0);
	const enabled = $derived(state.enabled && state.attachment === attachment && duration > 0 && validPosition);
	const total = $derived(duration ? formatDuration(duration) : null);

	function beginPointer(event: PointerEvent): void {
		if (enabled && event.isPrimary && event.button === 0 && !pointer) pointer = { id: event.pointerId, replay: true };
	}

	function input(event: Event & { currentTarget: HTMLInputElement }): void {
		const control = event.currentTarget;
		const requested = control.valueAsNumber;
		// Browser input is a request, not evidence. Restore the observed value
		// even if refusal leaves the owner's snapshot unchanged.
		control.valueAsNumber = value;
		if (!enabled || !Number.isFinite(requested) || requested < 0 || requested > duration) return;
		const replay = pointer?.replay ?? true;
		if (pointer) pointer.replay = false;
		seek(attachment, requested, replay);
	}

	function keydown(event: KeyboardEvent): void {
		if (!enabled || event.defaultPrevented || event.isComposing || event.keyCode === 229 ||
			event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
		let requested: number;
		switch (event.key) {
			case 'ArrowLeft': case 'ArrowDown': requested = value - 1; break;
			case 'ArrowRight': case 'ArrowUp': requested = value + 1; break;
			case 'PageDown': requested = value - duration / 10; break;
			case 'PageUp': requested = value + duration / 10; break;
			case 'Home': requested = 0; break;
			case 'End': requested = duration; break;
			default: return;
		}
		event.preventDefault();
		seek(attachment, Math.max(0, Math.min(duration, requested)), !event.repeat);
	}
</script>
<svelte:window onpointerup={endPointer} onpointercancel={endPointer} onblur={() => { pointer = undefined; }} />

<div class="nt-player-seek">
	<span aria-hidden="true">{elapsed}</span>
	<input type="range" data-testid="player-seek" aria-label="Seek"
		aria-valuetext={`${state.seeking ? `Seeking to ${formatDuration(value)}` : elapsed} of ${total ?? 'unknown duration'}`}
		min="0" max={duration || 1} step="any" {value} disabled={!enabled}
		onpointerdown={beginPointer} onlostpointercapture={endPointer} oninput={input} onkeydown={keydown} />
	<span aria-hidden="true">{total ?? '—'}</span>
</div>

<style>
	.nt-player-seek {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: center;
		gap: var(--nt-space-2);
		font-variant-numeric: tabular-nums;
	}
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
