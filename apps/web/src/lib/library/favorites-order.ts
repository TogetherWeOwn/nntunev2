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
