/**
 * Device-local favorites arrangement.
 *
 * Favorites persist as one ordered id array (see `favorites.ts`); only the
 * ids present in the current library render. Moving a favorite swaps it
 * with its visible neighbor in their existing slots of the complete
 * stored array, so absent ids keep their slots and reappear in place when
 * the library lists them again. Pure: no storage or network use.
 */

/** Move direction for a visible favorite within the stored order. */
export type FavoriteMoveDirection = 'up' | 'down';

/**
 * Anchor captured at removal time so a later Undo restores the removed id
 * near its original slot: the neighbors in the list as it was immediately
 * before this removal, plus the numeric index there as a last resort.
 */
export interface FavoriteRestoreAnchor {
	previousIndex: number;
	predecessor: string | null;
	successor: string | null;
}

/**
 * Reinsert a removed favorite `id` into the stored order. Successor first:
 * removals shift later elements left, so the surviving successor is the
 * stable right edge — restoring after it (removal order or reverse) keeps
 * adjacent Undos from swapping. Then the predecessor, then the clamped
 * index when both neighbors are gone. A present id is a no-op returning
 * `stored` itself, so callers can skip persistence with a reference check.
 * Never mutates its inputs.
 */
export function restoreFavoriteId(
	stored: readonly string[],
	id: string,
	anchor: FavoriteRestoreAnchor
): readonly string[] {
	if (stored.includes(id)) return stored;
	const next = [...stored];
	if (anchor.successor !== null) {
		const at = next.indexOf(anchor.successor);
		if (at >= 0) {
			next.splice(at, 0, id);
			return next;
		}
	}
	if (anchor.predecessor !== null) {
		const at = next.indexOf(anchor.predecessor);
		if (at >= 0) {
			next.splice(at + 1, 0, id);
			return next;
		}
	}
	next.splice(Math.min(Math.max(anchor.previousIndex, 0), next.length), 0, id);
	return next;
}

/**
 * Swap the visible favorite `id` with its visible neighbor in `direction`,
 * in their existing slots of the complete stored array. Ids absent from
 * `visibleIds` stay at their existing slots; membership is unchanged.
 * Never mutates its inputs. Any no-op — empty order, unknown or
 * non-visible id, or a boundary activation — returns `stored` itself, so
 * callers can skip persistence and announcements with a reference check.
 */
export function moveFavoriteId(
	stored: readonly string[],
	visibleIds: readonly string[],
	id: string,
	direction: FavoriteMoveDirection
): readonly string[] {
	const at = visibleIds.indexOf(id);
	const neighbor = direction === 'up' ? visibleIds[at - 1] : visibleIds[at + 1];
	if (at < 0 || neighbor === undefined) return stored;
	const from = stored.indexOf(id);
	const to = stored.indexOf(neighbor);
	if (from < 0 || to < 0) return stored;
	const next = [...stored];
	next[from] = neighbor;
	next[to] = id;
	return next;
}
