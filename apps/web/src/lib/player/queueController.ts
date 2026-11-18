import { writable } from 'svelte/store';
import { resolveStreamUrl } from '../library/stream-url.js';
import { currentTrack, type QueueState } from './queue.js';
import { createQueueRevision } from './queueRevision.js';

type LibrarySources = ReadonlyMap<string, ReadonlySet<string>>;
export interface QueuedPlaySource {
	streamUrl: string;
	isCurrent: () => boolean;
}

export type QueueCommitKind = 'replace' | 'move' | 'remove' | 'clear' | 'select' | 'trim';

/** Owns the route's queue and fences both queue operations and playback attempts. */
export function createQueueController(initial: QueueState) {
	let snapshot = initial;
	let librarySources: LibrarySources = new Map();
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
		begin(next: QueueState, kind: QueueCommitKind, withLibrary = false): number {
			const queueChanged = kind !== 'replace' || next !== snapshot;
			if (!queueChanged && !withLibrary) return operation;
			const receipt = ++operation;
			if (queueChanged) revision.revoke();
			return receipt;
		},
		prepareLibrary(items: readonly { id: string; streamUrl?: unknown }[]) {
			const sources = new Map<string, Set<string>>();
			for (const item of items) {
				const urls = sources.get(item.id) ?? new Set<string>();
				urls.add(resolveStreamUrl(item));
				sources.set(item.id, urls);
			}
			const currentId = currentTrack(snapshot)?.id;
			const tracks = snapshot.tracks.flatMap((track) => {
				const urls = sources.get(track.id);
				if (!urls) return [];
				return [!track.streamUrl || urls.has(track.streamUrl) ? track : { ...track, streamUrl: '' }];
			});
			if (tracks.length === snapshot.tracks.length &&
				tracks.every((track, index) => track === snapshot.tracks[index])) {
				return { next: snapshot, sources };
			}
			const selected = tracks.findIndex((track) => track.id === currentId);
			return { next: { tracks, currentIndex: tracks.length ? Math.max(0, selected) : -1 }, sources };
		},
		hasLibraryTrack(id: string): boolean {
			return alive && librarySources.has(id);
		},
		sourceForQueuedPlay(id: string, preferred?: string): QueuedPlaySource | null {
			const evidence = librarySources;
			const urls = evidence.get(id);
			if (!alive || !urls) return null;
			return {
				streamUrl: preferred !== undefined && urls.has(preferred) ? preferred : (urls.values().next().value ?? ''),
				isCurrent: () => alive && librarySources === evidence
			};
		},
		publish(next: QueueState, receipt: number, sources?: LibrarySources): void {
			if (!isCurrentOperation(receipt)) return;
			// Evidence is visible with the new queue, including to reentrant subscribers.
			if (sources) librarySources = sources;
			if (snapshot === next) return;
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
