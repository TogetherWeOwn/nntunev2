import { writable } from 'svelte/store';
import { resolveStreamUrl } from '../library/stream-url.js';
import type { SnapshotStorage } from '../library/cache.js';
import { createLibraryQueuePersistence } from './libraryQueuePersistence.js';
import { createQueue, currentTrack, type QueueState, type Track } from './queue.js';
import { createQueueRevision } from './queueRevision.js';

export type LibrarySources = ReadonlyMap<string, ReadonlySet<string>>;
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
	let selectionGeneration = 0;
	let alive = true;
	let purging = false;
	const revision = createQueueRevision();
	const store = writable(initial);
	const persistence = createLibraryQueuePersistence();
	let libraryBinding: string | undefined;

	function isCurrentOperation(receipt: number): boolean {
		return alive && !purging && operation === receipt;
	}

	return {
		get snapshot() { return snapshot; },
		get selectionGeneration() { return selectionGeneration; },
		get purging() { return purging; },
		get revision() { return revision.revision; },
		subscribe: store.subscribe,
		// The layout can outlive the route and its refresh receipts.
		setLibraryBinding(binding: string): boolean {
			const replaced = libraryBinding !== undefined && libraryBinding !== binding;
			libraryBinding = binding;
			return replaced;
		},
		restoreLibraryQueue: (storage: SnapshotStorage, catalogue: () => Track[]) =>
			alive && !purging ? persistence.restore(storage, snapshot, catalogue) : null,
		clearSavedLibraryQueue: persistence.clear,
		begin(next: QueueState, kind: QueueCommitKind, withLibrary = false): number {
			if (!alive || purging) return -1;
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
			// Refresh controls membership and fallback evidence, not a surviving slot's source.
			const tracks = snapshot.tracks.filter((track) => sources.has(track.id));
			if (tracks.length === snapshot.tracks.length) {
				return { next: snapshot, sources };
			}
			const selected = tracks.findIndex((track) => track.id === currentId);
			return { next: { tracks, currentIndex: tracks.length ? Math.max(0, selected) : -1 }, sources };
		},
		hasLibraryTrack(id: string): boolean {
			return alive && librarySources.has(id);
		},
		hasLibrarySource(id: string, streamUrl: string): boolean {
			return alive && librarySources.get(id)?.has(streamUrl) === true;
		},
		sourceForQueuedPlay(id: string, preferred?: string): QueuedPlaySource | null {
			const evidence = librarySources;
			const urls = evidence.get(id);
			if (!alive || !urls) return null;
			const usePreferred = preferred !== undefined && (urls.has(preferred) ||
				(preferred !== '' && snapshot.tracks.some((track) => track.id === id && resolveStreamUrl(track) === preferred)));
			return {
				streamUrl: usePreferred ? preferred : (urls.values().next().value ?? ''),
				isCurrent: () => alive && librarySources === evidence
			};
		},
		publish(next: QueueState, receipt: number, sources?: LibrarySources): void {
			if (!isCurrentOperation(receipt)) return;
			// Evidence is visible with the new queue, including to reentrant subscribers.
			if (sources) librarySources = sources;
			if (snapshot === next) return;
			if (currentTrack(snapshot)?.id !== currentTrack(next)?.id) selectionGeneration++;
			snapshot = next;
			store.set(next);
			if (isCurrentOperation(receipt)) persistence.save(next);
		},
		purgeAtAuthBoundary(retirePlayer: () => void): void {
			if (!alive || purging) return;
			purging = true;
			libraryBinding = undefined;
			persistence.clear();
			operation += 1;
			revision.revoke();
			librarySources = new Map();
			const previous = snapshot;
			selectionGeneration++;
			if (previous.tracks.length || previous.currentIndex !== -1) snapshot = createQueue([]);
			// Native cleanup sees the empty snapshot; subscribers only see it after cleanup.
			try {
				retirePlayer();
			} finally {
				try {
					if (previous !== snapshot) store.set(snapshot);
				} finally {
					purging = false;
				}
			}
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
