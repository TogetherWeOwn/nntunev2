export function selectedRowRecoveryIds(
	displayedIds: readonly string[],
	removedId: string,
	selectedIds: readonly string[]
): { nextId: string | null; previousId: string | null } {
	const removedIndex = displayedIds.indexOf(removedId);
	const survivors = new Set(selectedIds);
	survivors.delete(removedId);
	return {
		nextId: removedIndex < 0
			? null
			: (displayedIds.slice(removedIndex + 1).find((id) => survivors.has(id)) ?? null),
		previousId: removedIndex <= 0
			? null
			: (displayedIds.slice(0, removedIndex).findLast((id) => survivors.has(id)) ?? null)
	};
}
