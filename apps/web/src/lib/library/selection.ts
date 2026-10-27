/**
 * Explicit library-subset selection: pure, page-local helpers.
 *
 * The library page keeps an ordered `string[]` of stable track IDs in
 * checkbox activation order: no duplicates, no empty/non-string entries.
 * IDs compare exactly (case-sensitive), matching the keyed list and the
 * snapshot dedupe discipline. All helpers are pure: no storage, network,
 * clock or randomness, and no `window` required.
 */

import type { LibraryItem } from './cache.js';
import { displayArtist, displayTitle } from './order.js';
import { addTrack, type QueueState } from '../player/queue.js';

export interface SelectionAppendResult {
	queue: QueueState;
	added: number;
	skipped: number;
}

/** A fresh empty selection. */
export function createSelection(): string[] {
	return [];
}

function isValidId(id: unknown): id is string {
	return typeof id === 'string' && id.length > 0;
}

/**
 * Toggle one ID: a present ID is removed (all occurrences, so duplicate
 * inputs self-heal); an absent ID is appended at the end. An invalid `id`
 * is a no-op returning the same reference. Changes return a new array;
 * the input is never mutated.
 */
export function toggleSelected(selection: readonly string[], id: unknown): string[] {
	if (!isValidId(id)) return selection as string[];
	if ((selection as readonly unknown[]).includes(id)) {
		return (selection as readonly unknown[]).filter((entry) => entry !== id) as string[];
	}
	return [...selection, id];
}

/** Empty the selection; the input is never mutated. */
export function clearSelection(selection: readonly string[]): string[] {
	void selection;
	return [];
}

/**
 * Keep selected IDs present in `loadedIds` (exact match, duplicates in
 * `loadedIds` ignored), preserving activation order — never loaded order.
 * Drops non-string/empty entries from the selection. A no-op (nothing
 * dropped, nothing sanitized) returns the same reference; otherwise a new
 * array. Inputs are never mutated.
 */
export function pruneSelection(
	selection: readonly string[],
	loadedIds: readonly string[]
): string[] {
	const loaded = new Set<string>();
	for (const id of loadedIds) {
		if (isValidId(id)) loaded.add(id);
	}
	const kept: string[] = [];
	let changed = false;
	for (const entry of selection as readonly unknown[]) {
		if (!isValidId(entry) || !loaded.has(entry)) {
			changed = true;
			continue;
		}
		kept.push(entry);
	}
	if (!changed) return selection as string[];
	return kept;
}

/**
 * Project `items` to the rows whose ID is selected: exact case-sensitive
 * membership, input-list order (never activation order), the same row
 * references in a new array. Unknown/pruned, duplicate, non-string and
 * empty selected entries contribute nothing; loaded rows sharing a
 * selected ID all survive (dedupe stays the append path's job). Pure and
 * nonmutating, so the library page composes it with the existing find
 * and ordering steps as the single Selected-only projection.
 */
export function selectSelectedItems(
	items: readonly LibraryItem[],
	selectedIds: readonly string[]
): LibraryItem[] {
	const selected = new Set<string>();
	for (const entry of selectedIds as readonly unknown[]) {
		if (isValidId(entry)) selected.add(entry);
	}
	return items.filter((item) => selected.has(item.id));
}

/** Count valid selected IDs absent from `visibleIds` (exact match). */
export function hiddenSelectedCount(
	selection: readonly string[],
	visibleIds: readonly string[]
): number {
	const visible = new Set<string>();
	for (const id of visibleIds) {
		if (isValidId(id)) visible.add(id);
	}
	let hidden = 0;
	for (const entry of selection as readonly unknown[]) {
		if (!isValidId(entry)) continue;
		if (!visible.has(entry)) hidden += 1;
	}
	return hidden;
}

/**
 * Append current loaded metadata for the selection in activation order
 * through the existing queue dedupe semantics (first loaded occurrence
 * wins; already-queued IDs are skipped, never duplicated). Missing IDs
 * are ignored — neither added nor counted as skipped. New tracks carry
 * `{ id, title, artist, streamUrl: '' }` (the metadata-only contract).
 * Existing entries, order and current track are preserved; an empty queue
 * selects its first added ID under current queue semantics. Returns
 * `{ queue, added, skipped }`; when `added` is 0 the same queue
 * reference returns. Duplicate selection inputs append once (first
 * occurrence). Inputs are never mutated.
 */
export function appendSelection(
	queue: QueueState,
	selection: readonly string[],
	loaded: readonly LibraryItem[]
): SelectionAppendResult {
	const byId = new Map<string, LibraryItem>();
	for (const item of loaded) {
		if (!byId.has(item.id)) byId.set(item.id, item);
	}
	let next = queue;
	let added = 0;
	let skipped = 0;
	const seen = new Set<string>();
	for (const entry of selection as readonly unknown[]) {
		if (!isValidId(entry) || seen.has(entry)) continue;
		seen.add(entry);
		const item = byId.get(entry);
		if (!item) continue;
		if (next.tracks.some((track) => track.id === entry)) {
			skipped += 1;
			continue;
		}
		next = addTrack(next, {
			id: item.id,
			title: displayTitle(item),
			artist: displayArtist(item) ?? '',
			streamUrl: ''
		});
		added += 1;
	}
	return { queue: next, added, skipped };
}

/** `${count} selected` for a finite integer `count >= 0`, else `0 selected`. */
export function selectionCountLabel(count: number): string {
	const valid = typeof count === 'number' && Number.isInteger(count) && count >= 0;
	return `${valid ? count : 0} selected`;
}

/** `Added <a> <track|tracks> (<s> already queued)` with exact plural. */
export function appendResultLabel(added: number, skipped: number): string {
	return `Added ${added} ${added === 1 ? 'track' : 'tracks'} (${skipped} already queued)`;
}
