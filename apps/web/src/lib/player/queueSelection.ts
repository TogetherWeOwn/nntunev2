import { pruneSelection, selectAllDisplayed, selectRange, toggleSelected } from '../library/selection.js';
import { matchesQueueQuery } from './queueFilter.js';
import { currentTrack, type QueueState } from './queue.js';

export interface QueueSelection {
	ids: string[];
	anchorId: string | null;
	active: boolean;
}

export function createQueueSelection(): QueueSelection {
	return { ids: [], anchorId: null, active: false };
}

function displayedIds(queue: QueueState, query: string): string[] {
	return queue.tracks.filter((track) => matchesQueueQuery(track, query)).map((track) => track.id);
}

/** Hidden selections survive filtering; a hidden or removed range anchor does not. */
export function reconcileQueueSelection(selection: QueueSelection, queue: QueueState, query: string): QueueSelection {
	const ids = pruneSelection(selection.ids, queue.tracks.map((track) => track.id));
	const anchorId = selection.anchorId !== null && displayedIds(queue, query).includes(selection.anchorId)
		? selection.anchorId : null;
	const active = selection.active && queue.tracks.length > 0;
	return ids === selection.ids && anchorId === selection.anchorId && active === selection.active
		? selection : { ids, anchorId, active };
}

/** Resolve gestures against the current projection, never stale row coordinates. */
export function toggleQueueSelection(selection: QueueSelection, queue: QueueState, query: string, id: string, range = false): QueueSelection {
	const displayed = displayedIds(queue, query);
	if (!displayed.includes(id)) return selection;
	const ids = range ? selectRange(selection.ids, displayed, selection.anchorId, id) : toggleSelected(selection.ids, id);
	const anchorId = range && selection.anchorId !== null && displayed.includes(selection.anchorId)
		? selection.anchorId : id;
	return ids === selection.ids && anchorId === selection.anchorId && selection.active
		? selection : { ids, anchorId, active: true };
}

export type QueueSelectionAction = 'remove' | 'playNext';

/** One arranged-order edit over stable IDs, including selected rows hidden by the filter. */
export function applyQueueSelection(queue: QueueState, ids: readonly string[], action: QueueSelectionAction): QueueState {
	const selected = new Set(ids);
	const current = currentTrack(queue);
	if (action === 'playNext' && !current) return queue;
	const remaining = queue.tracks.filter((track) => !selected.has(track.id) || (action === 'playNext' && track === current));
	if (remaining.length === queue.tracks.length) return queue;
	let currentIndex = queue.tracks.slice(0, queue.currentIndex).filter((track) => !selected.has(track.id)).length;
	if (action === 'remove') {
		currentIndex = current ? Math.min(currentIndex, remaining.length - 1) : -1;
		return { tracks: remaining, currentIndex };
	}
	const tracks = [
		...remaining.slice(0, currentIndex + 1),
		...queue.tracks.filter((track) => selected.has(track.id) && track !== current),
		...remaining.slice(currentIndex + 1)
	];
	return tracks.every((track, index) => track === queue.tracks[index]) ? queue : { tracks, currentIndex };
}

/** Select all means this queue's displayed rows, preserving hidden membership. */
export function selectAllQueueRows(selection: QueueSelection, queue: QueueState, query: string): QueueSelection {
	const displayed = displayedIds(queue, query);
	if (!displayed.length) return selection;
	const ids = selectAllDisplayed(selection.ids, displayed);
	if (selection.active && ids.length === selection.ids.length) return selection;
	return { ...selection, ids, active: true };
}
