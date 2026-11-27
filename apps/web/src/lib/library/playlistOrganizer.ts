type ItemRef = Readonly<{ ownerId: string; id: string }>;

// Synthetic UI input. Playlist identifiers and names mirror the private list
// contract. Folder placement is local metadata and carries no track access.
type OrganizerItem = ItemRef & Readonly<{
	kind: 'folder' | 'playlist';
	name: string;
	parentId: string | null;
}>;

type OrganizerState = Readonly<{
	ownerId: string;
	items: readonly OrganizerItem[];
	shareSheet: Readonly<{ playlistId: string; visibility: 'private' }> | null;
}>;

type OrganizerAction =
	| { type: 'create-folder'; folder: ItemRef; name: string; parent: ItemRef | null }
	| { type: 'rename-folder'; folder: ItemRef; name: string }
	| { type: 'delete-folder'; folder: ItemRef }
	| { type: 'move'; item: ItemRef; destination: ItemRef | null }
	| { type: 'open-share-sheet'; playlist: ItemRef }
	| { type: 'cancel-share-sheet' };

const invalidText = /[\p{Cc}\p{Cs}\u2028\u2029]/u;

function isRecord(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function isId(value: unknown): value is string {
	return typeof value === 'string' && value.length > 0 && value === value.trim() && !invalidText.test(value);
}

function isOwnedRef(value: unknown, ownerId: string): value is ItemRef {
	return isRecord(value) && value.ownerId === ownerId && isId(value.id);
}

function isPlaylistName(name: string): boolean {
	// The API trims stored names and permits interior whitespace, including line breaks.
	return /^.{1,120}$/su.test(name) && /\P{White_Space}/u.test(name) &&
		!name.includes('\0') && !/\p{Cs}/u.test(name);
}

/** Create or reset the local model from a synthetic owner snapshot. */
export function createPlaylistOrganizer(
	ownerId: string,
	rows: unknown
): OrganizerState | null {
	if (!isId(ownerId) || !Array.isArray(rows)) return null;
	const items: OrganizerItem[] = [];
	const byId = new Map<string, OrganizerItem>();
	for (const row of rows as readonly unknown[]) {
		// Only skip foreign rows once their owner is known.
		if (!isRecord(row) || !isId(row.ownerId)) return null;
		if (row.ownerId !== ownerId) continue;
		const { id, kind, name, parentId } = row;
		if (!isId(id) || byId.has(id) || (kind !== 'folder' && kind !== 'playlist') ||
			(parentId !== null && !isId(parentId)) || typeof name !== 'string') return null;
		if (kind === 'playlist' ? !isPlaylistName(name) : normalizeFolderName(name) === null) return null;
		const item = { ownerId, id, kind, name, parentId } as const;
		items.push(item);
		byId.set(id, item);
	}

	// Check each ancestor chain once. A parent ID is relative to this owner.
	const checked = new Set<string>();
	for (const item of items) {
		const path = new Set([item.id]);
		let parentId = item.parentId;
		while (parentId !== null) {
			const parent = byId.get(parentId);
			if (parent?.kind !== 'folder' || path.has(parentId)) return null;
			if (checked.has(parentId)) break;
			path.add(parentId);
			parentId = parent.parentId;
		}
		for (const id of path) checked.add(id);
	}
	return { ownerId, items, shareSheet: null };
}

/** Keep IDs and snapshot order when changing a single parent. */
function moveItem(state: OrganizerState, itemRef: ItemRef, destination: ItemRef | null): OrganizerState {
	if (!isOwnedRef(itemRef, state.ownerId) || (destination !== null && !isOwnedRef(destination, state.ownerId))) {
		return state;
	}
	const byId = new Map(state.items.map((item) => [item.id, item]));
	const item = byId.get(itemRef.id);
	const parentId = destination?.id ?? null;
	if (!item || item.parentId === parentId) return state;

	let ancestorId = parentId;
	while (ancestorId !== null) {
		const ancestor = byId.get(ancestorId);
		if (ancestor?.kind !== 'folder' || ancestorId === item.id) return state;
		ancestorId = ancestor.parentId;
	}
	return {
		...state,
		items: state.items.map((entry) => entry === item ? { ...entry, parentId } : entry)
	};
}

function normalizeFolderName(name: unknown): string | null {
	// Check before trimming so controls and line separators at the edges are rejected too.
	if (typeof name !== 'string' || invalidText.test(name)) return null;
	const trimmed = name.trim();
	// Use the Library's 120-code-point name limit.
	return /^.{1,120}$/u.test(trimmed) ? trimmed : null;
}

function findOwnedFolder(state: OrganizerState, folder: ItemRef): OrganizerItem | undefined {
	if (!isOwnedRef(folder, state.ownerId)) return undefined;
	return state.items.find((item) =>
		item.id === folder.id && item.ownerId === folder.ownerId && item.kind === 'folder'
	);
}

export function updatePlaylistOrganizer(state: OrganizerState, action: OrganizerAction): OrganizerState {
	if (!isRecord(action) || !Object.hasOwn(action, 'type')) return state;
	switch (action.type) {
		case 'create-folder': {
			const { folder, parent } = action;
			if (!isOwnedRef(folder, state.ownerId) || state.items.some((item) => item.id === folder.id)) return state;
			if (parent !== null && !findOwnedFolder(state, parent)) return state;
			const name = normalizeFolderName(action.name);
			if (name === null) return state;
			const { ownerId, id } = folder;
			return {
				...state,
				items: [...state.items, { ownerId, id, name, kind: 'folder', parentId: parent?.id ?? null }]
			};
		}
		case 'rename-folder': {
			const folder = findOwnedFolder(state, action.folder);
			if (!folder) return state;
			const name = normalizeFolderName(action.name);
			if (name === null || name === folder.name) return state;
			return {
				...state,
				items: state.items.map((item) => item === folder ? { ...item, name } : item)
			};
		}
		case 'delete-folder': {
			const folder = findOwnedFolder(state, action.folder);
			if (!folder || state.items.some((item) => item.parentId === folder.id)) return state;
			return { ...state, items: state.items.filter((item) => item !== folder) };
		}
		case 'move':
			return moveItem(state, action.item, action.destination);
		case 'open-share-sheet': {
			if (!isOwnedRef(action.playlist, state.ownerId)) return state;
			const { id, ownerId } = action.playlist;
			if (state.shareSheet?.playlistId === id) return state;
			const playlist = state.items.find((item) => item.kind === 'playlist' && item.id === id && item.ownerId === ownerId);
			return playlist ? { ...state, shareSheet: { playlistId: id, visibility: 'private' } } : state;
		}
		case 'cancel-share-sheet':
			return state.shareSheet ? { ...state, shareSheet: null } : state;
		default:
			return state;
	}
}
