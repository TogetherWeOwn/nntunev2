<script lang="ts">
	import PlayerQueue from '../../../src/lib/player/PlayerQueue.svelte';
	import { createQueue, type QueueState } from '../../../src/lib/player/queue.js';

	// Test parent only: replace props on one mounted child, without any new
	// production input/export. The sole existing transport callback is observed.
	let queueValue: QueueState = $state(createQueue([]));
	let nextCalls = 0;

	export function replace(value: QueueState) {
		queueValue = value;
	}

	export function inspect() {
		return { state: $state.snapshot(queueValue), nextCalls };
	}
</script>

<button type="button" data-testid="focus-sentinel">Keep focus here</button>
<PlayerQueue state={queueValue} onnext={() => { nextCalls += 1; }} />

<style>
	/* A long queue and an always-visible control make offscreen reveal
	 * deterministic without prescribing the product's control placement. */
	:global([data-testid='queue-item']) { min-height: 72px; }
	:global([data-testid='queue-reveal-current']) { position: fixed; top: 8px; right: 8px; }
</style>
