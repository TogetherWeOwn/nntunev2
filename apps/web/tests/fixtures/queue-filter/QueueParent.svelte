<script lang="ts">
	import PlayerQueue from '../../../src/lib/player/PlayerQueue.svelte';
	import { createQueue, type QueueState } from '../../../src/lib/player/queue.js';
	import type { QueueFilterCall } from './mount.js';

	// Test boundary observer only: real component, frozen real QueueState,
	// existing public callbacks. No matching/reordering/transport implementation.
	let value: QueueState = $state(createQueue([]));
	let calls: QueueFilterCall[] = [];

	export function replace(next: QueueState) { value = next; }
	export function inspect() { return { state: $state.snapshot(value), calls: [...calls] }; }
	function record(action: string, index?: number, to?: number) {
		calls.push({ action, ...(index === undefined ? {} : { index, id: value.tracks[index]?.id }), ...(to === undefined ? {} : { to }) });
	}
</script>

<button type="button" data-testid="focus-sentinel">Keep focus here</button>
<PlayerQueue
	state={value}
	onnext={() => record('next')}
	onshuffle={() => record('shuffle')}
	onmove={(from, to) => record('move', from, to)}
	onremove={(index) => record('remove', index)}
	onplaynext={(index) => record('play-next', index)}
	onclear={() => record('clear')}
/>
