<script lang="ts">
	import PlayerQueue from '../../../src/lib/player/PlayerQueue.svelte';
	import { createQueue, type QueueState } from '../../../src/lib/player/queue.js';

	// Test host deliberately does not apply the callback: the real child must
	// report original coordinates without taking ownership of state/transport.
	let queueValue: QueueState = $state(createQueue([]));
	let enabled = $state(true);
	let calls: number[] = [];
	let nextCalls = 0;
	function selected(index: number) { calls.push(index); }
	const selectionProps = $derived({ onselect: enabled ? selected : undefined });

	export function replace(value: QueueState, available = true) {
		queueValue = value;
		enabled = available;
		calls = [];
		nextCalls = 0;
	}

	export function inspect() {
		return { state: $state.snapshot(queueValue), calls: [...calls], nextCalls };
	}
</script>

<button type="button" data-testid="pick-sentinel">Keep focus here</button>
<PlayerQueue state={queueValue} {...selectionProps} onnext={() => { nextCalls += 1; }} />
