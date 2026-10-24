<script lang="ts">
	import PlayerQueue from '../../../src/lib/player/PlayerQueue.svelte';
	import { createQueue, type QueueState } from '../../../src/lib/player/queue.js';

	// This host records intent, deliberately does not implement any transition.
	// Raw state preserves supplied object identities and frozen input invariants.
	let queueValue: QueueState = $state.raw(createQueue([]));
	let supplied = $state(true);
	let calls: string[] = [];

	export function replace(value: QueueState, callbacks = true) {
		queueValue = value;
		supplied = callbacks;
		calls = [];
	}

	export function inspect() {
		return { state: queueValue, calls: [...calls] };
	}

	const callbacks = $derived(supplied ? {
		onkeepcurrentandupcoming: () => { calls.push('keep'); },
		onclearupcoming: () => { calls.push('clear-upcoming'); }
	} : {});
</script>

<button type="button" data-testid="trim-sentinel">Keep focus here</button>
<PlayerQueue
	state={queueValue}
	{...callbacks}
	onnext={() => { calls.push('next'); }}
	onshuffle={() => { calls.push('shuffle'); }}
	onmove={() => { calls.push('move'); }}
	onplaynext={() => { calls.push('play-next'); }}
	onremove={() => { calls.push('remove'); }}
	onclear={() => { calls.push('clear-all'); }}
/>
