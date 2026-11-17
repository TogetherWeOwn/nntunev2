import { writable } from 'svelte/store';
import type { QueueState } from './queue.js';
import { createQueueRevision } from './queueRevision.js';

export type QueueCommitKind = 'replace' | 'move' | 'remove' | 'clear' | 'select' | 'trim';

/** Owns the route's queue and fences both queue operations and playback attempts. */
export function createQueueController(initial: QueueState) {
	let snapshot = initial;
	let operation = 0;
	let alive = true;
	const revision = createQueueRevision();
	const store = writable(initial);

	function isCurrentOperation(receipt: number): boolean {
		return alive && operation === receipt;
	}

	return {
		get snapshot() { return snapshot; },
		get revision() { return revision.revision; },
		subscribe: store.subscribe,
		begin(next: QueueState, kind: QueueCommitKind): number {
			const receipt = ++operation;
			if (kind !== 'replace' || next !== snapshot) revision.revoke();
			return receipt;
		},
		publish(next: QueueState, receipt: number): void {
			if (!isCurrentOperation(receipt)) return;
			snapshot = next;
			store.set(next);
		},
		isCurrentOperation,
		revoke: revision.revoke,
		isCurrent: revision.isCurrent,
		destroy(): void {
			alive = false;
			operation += 1;
			revision.destroy();
		}
	};
}
