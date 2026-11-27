type Identified = { id: string };

export function projectInteractionIds(id: string, selected: readonly string[], rendered: readonly Identified[]): string[] {
	if (!rendered.some((item) => item.id === id)) return [];
	return selected.includes(id)
		? [...new Set(rendered.filter((item) => selected.includes(item.id)).map((item) => item.id))]
		: [id];
}

export function survivingInteractionIds(ids: readonly string[], available: readonly Identified[], unavailable: boolean): string[] {
	if (unavailable) return [];
	return [...new Set(ids)].filter((id) => available.some((item) => item.id === id));
}
