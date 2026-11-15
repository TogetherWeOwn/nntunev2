import type { QueueState } from '../queue.js';

/** Real rune assignment semantics for extracted-handler tests, not a production owner. */
export function reactiveQueue(initial: QueueState) {
	let queue = $state(initial);
	return {
		get queue() { return queue; },
		set queue(next: QueueState) { queue = next; }
	};
}
