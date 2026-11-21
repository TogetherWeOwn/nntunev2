<script lang="ts">
	import Icon from '../design/Icon.svelte';
	import type { TrackLikeControl } from './track-like-controls.js';

	let { title, control }: { title: string; control: TrackLikeControl } = $props();
</script>

<button
	type="button"
	class="nt-track-like"
	aria-label={`Like ${title}`}
	aria-pressed={control.liked}
	aria-disabled={control.pending}
	onclick={() => { if (!control.pending) void control.toggle(); }}
>
	<Icon name={control.liked ? 'heart-filled' : 'heart'} />
</button>

<style>
	.nt-track-like {
		display: inline-grid;
		place-items: center;
		flex: none;
		min-width: 44px;
		min-height: 44px;
		border: 0;
		background: transparent;
		color: inherit;
		cursor: pointer;
		vertical-align: middle;
	}
	.nt-track-like[aria-pressed='true'] { color: var(--nt-accent, currentColor); }
	.nt-track-like[aria-disabled='true'] { cursor: wait; }
	.nt-track-like:focus-visible { outline: 2px solid var(--nt-accent, currentColor); outline-offset: 2px; }
	@media (pointer: coarse) {
		.nt-track-like { min-width: 48px; min-height: 48px; }
	}
</style>
