import { describe, expect, it } from 'vitest';
import { createPlaylistOrganizer, updatePlaylistOrganizer } from './playlistOrganizer.js';

const ownerId = 'owner-a';
const otherOwner = 'owner-b';
const own = (id: string) => ({ ownerId, id });
const foreign = (id: string) => ({ ownerId: otherOwner, id });
const items = [
	{ ...own('nested'), name: 'Nested', kind: 'folder', parentId: 'root' },
	{ ...own('playlist'), name: 'Quiet morning', kind: 'playlist', parentId: 'nested' },
	{ ...own('root'), name: 'Collection', kind: 'folder', parentId: null },
	{ ...own('second'), name: 'Afternoon', kind: 'playlist', parentId: null }
] as const;
const otherItems = [
	{ ...foreign('root'), name: 'Other owner root', kind: 'folder', parentId: null },
	{ ...foreign('playlist'), name: 'Other owner playlist', kind: 'playlist', parentId: 'root' },
	{ ...foreign('foreign-only'), name: 'Other owner folder', kind: 'folder', parentId: null }
] as const;
const snapshot = [...items, ...otherItems];

function setup() {
	const state = createPlaylistOrganizer(ownerId, snapshot);
	if (!state) throw new Error('Expected a valid owner snapshot');
	return state;
}

describe('playlist organizer', () => {
	it('initializes the owner hierarchy in stable order with only UI metadata', () => {
		const first = { ...items[0], name: 'Nested', entries: [{ id: 'unused-entry', dormant: true }] };
		const state = createPlaylistOrganizer(ownerId, [first, ...snapshot.slice(1)]);
		expect(state).toEqual({ ownerId, items, shareSheet: null });
		first.name = 'Changed fixture';
		expect(state?.items[0].name).toBe('Nested');
	});

	it('rejects duplicate nodes, non-folder parents, missing or foreign parents, and cycles in the snapshot', () => {
		const nested = items[0];
		for (const parentId of ['playlist', 'missing', 'foreign-only', 'nested']) {
			expect(createPlaylistOrganizer(ownerId, [...snapshot.slice(1), { ...nested, parentId }])).toBeNull();
		}
		expect(createPlaylistOrganizer(ownerId, [...snapshot, nested])).toBeNull();
		expect(createPlaylistOrganizer(ownerId, [nested, { ...items[2], parentId: 'nested' }])).toBeNull();
	});

	it('moves a subtree and a playlist deterministically while preserving IDs and row order', () => {
		const initial = setup();
		const action = { type: 'move', item: own('nested'), destination: null } as const;
		const movedFolder = updatePlaylistOrganizer(initial, action);
		expect(movedFolder).toEqual(updatePlaylistOrganizer(initial, action));
		expect(updatePlaylistOrganizer(movedFolder, action)).toBe(movedFolder);
		expect(movedFolder.items).toEqual([{ ...items[0], parentId: null }, ...items.slice(1)]);
		expect(movedFolder.items[1]).toBe(initial.items[1]);

		const movedPlaylist = updatePlaylistOrganizer(movedFolder, {
			type: 'move', item: own('playlist'), destination: own('root')
		});
		expect(movedPlaylist.items).toEqual([
			{ ...items[0], parentId: null }, { ...items[1], parentId: 'root' }, ...items.slice(2)
		]);
		expect(initial.items).toEqual(items);
	});

	it('refuses cycles, missing nodes, playlist parents, and cross-owner moves atomically', () => {
		const state = setup();
		const rejected = [
			[own('root'), own('root')],
			[own('root'), own('nested')],
			[own('nested'), own('playlist')],
			[own('playlist'), own('missing')],
			[own('missing'), null],
			[own('playlist'), foreign('root')],
			[foreign('playlist'), own('root')]
		] as const;
		for (const [item, destination] of rejected) {
			expect(updatePlaylistOrganizer(state, { type: 'move', item, destination })).toBe(state);
		}
		expect(state.items).toEqual(items);
	});

	it('opens and retargets the private sheet only for known owned playlists', () => {
		const state = setup();
		const open = (playlist: ReturnType<typeof own>, from = state) =>
			updatePlaylistOrganizer(from, { type: 'open-share-sheet', playlist });
		const opened = open(own('playlist'));
		expect(opened.shareSheet).toEqual({ playlistId: 'playlist', visibility: 'private' });
		expect(open(own('playlist'), opened)).toBe(opened);
		expect(open(own('root'), opened)).toBe(opened);
		expect(open(own('missing'), opened)).toBe(opened);
		expect(open(foreign('playlist'), opened)).toBe(opened);
		const retargeted = open(own('second'), opened);
		expect(retargeted.shareSheet).toEqual({ playlistId: 'second', visibility: 'private' });
		expect(retargeted.items).toBe(state.items);
	});

	it('cancels without reverting moves and resets placement and sheet from a new snapshot', () => {
		const initial = setup();
		const moved = updatePlaylistOrganizer(initial, { type: 'move', item: own('playlist'), destination: null });
		const opened = updatePlaylistOrganizer(moved, { type: 'open-share-sheet', playlist: own('playlist') });
		const cancelled = updatePlaylistOrganizer(opened, { type: 'cancel-share-sheet' });
		expect(cancelled.shareSheet).toBeNull();
		expect(cancelled.items).toBe(moved.items);
		expect(cancelled.items[1].parentId).toBeNull();
		expect(updatePlaylistOrganizer(cancelled, { type: 'cancel-share-sheet' })).toBe(cancelled);
		expect(createPlaylistOrganizer(ownerId, snapshot)).toEqual(initial);

		const reset = createPlaylistOrganizer(otherOwner, snapshot);
		if (!reset) throw new Error('Expected a valid replacement snapshot');
		expect(reset).toEqual({ ownerId: otherOwner, items: otherItems, shareSheet: null });
		expect(updatePlaylistOrganizer(reset, { type: 'open-share-sheet', playlist: own('playlist') })).toBe(reset);
	});
});

describe('playlist organizer folder actions', () => {
	it('creates root and nested folders in insertion order with trimmed names', () => {
		const initial = updatePlaylistOrganizer(setup(), {
			type: 'open-share-sheet', playlist: own('playlist')
		});
		const root = updatePlaylistOrganizer(initial, {
			type: 'create-folder', folder: own('new-root'), name: '  Late  night  ', parent: null
		});
		const nested = updatePlaylistOrganizer(root, {
			type: 'create-folder', folder: own('foreign-only'), name: '\u00a0Écoute 🎵\u00a0',
			parent: own('new-root')
		});

		expect(root.items).toEqual([
			...items, { ...own('new-root'), kind: 'folder', name: 'Late  night', parentId: null }
		]);
		expect(nested.items).toEqual([
			...root.items,
			{ ...own('foreign-only'), kind: 'folder', name: 'Écoute 🎵', parentId: 'new-root' }
		]);
		for (const [index, item] of root.items.entries()) expect(nested.items[index]).toBe(item);
		for (const [index, item] of initial.items.entries()) expect(root.items[index]).toBe(item);
		expect(initial.items).toEqual(items);
		expect(root.shareSheet).toBe(initial.shareSheet);
		expect(nested.shareSheet).toBe(initial.shareSheet);
	});

	it('refuses duplicate IDs, cross-owner creates, and invalid parents atomically', () => {
		const state = setup();
		const rejected = [
			[own('root'), null],
			[own('playlist'), null],
			[foreign('new'), null],
			[own('new'), foreign('root')],
			[own('new'), own('foreign-only')],
			[own('new'), own('missing')],
			[own('new'), own('playlist')],
			[own('new'), own('new')]
		] as const;
		for (const [folder, parent] of rejected) {
			expect(updatePlaylistOrganizer(state, {
				type: 'create-folder', folder, parent, name: 'New folder'
			})).toBe(state);
		}
		expect(state.items).toEqual(items);
	});

	it.each([
		['empty', ''],
		['whitespace only', ' \u00a0 '],
		['above the name limit', '🎵'.repeat(121)],
		['a NUL', 'Side\0 A'],
		['a tab', '\tSide A'],
		['a newline', 'Side A\n'],
		['a delete control', 'Side\u007f A'],
		['a C1 control', 'Side\u0085 A'],
		['a line separator', '\u2028Side A'],
		['a paragraph separator', 'Side A\u2029']
	])('rejects %s for both create and rename', (_label, name) => {
		const state = setup();
		expect(updatePlaylistOrganizer(state, {
			type: 'create-folder', folder: own('new'), parent: null, name
		})).toBe(state);
		expect(updatePlaylistOrganizer(state, {
			type: 'rename-folder', folder: own('root'), name
		})).toBe(state);
		expect(state.items).toEqual(items);
	});

	it('accepts 120 Unicode code points after trimming for both create and rename', () => {
		const state = setup();
		const name = '🎵'.repeat(120);
		const created = updatePlaylistOrganizer(state, {
			type: 'create-folder', folder: own('new'), parent: null, name: ` ${name} `
		});
		const renamed = updatePlaylistOrganizer(state, {
			type: 'rename-folder', folder: own('nested'), name: ` ${name} `
		});
		expect(created.items[items.length]?.name).toBe(name);
		expect(renamed.items[0].name).toBe(name);
	});

	it('renames a populated folder in place while preserving its children and the private sheet', () => {
		const state = updatePlaylistOrganizer(setup(), {
			type: 'open-share-sheet', playlist: own('playlist')
		});
		const renamed = updatePlaylistOrganizer(state, {
			type: 'rename-folder', folder: own('nested'), name: '  Evening  '
		});

		expect(renamed.items).toEqual([{ ...items[0], name: 'Evening' }, ...items.slice(1)]);
		expect(renamed.items[0]).not.toBe(state.items[0]);
		for (const [index, item] of renamed.items.entries()) {
			if (index !== 0) expect(item).toBe(state.items[index]);
		}
		expect(renamed.shareSheet).toBe(state.shareSheet);
		expect(state.items).toEqual(items);
	});

	it('refuses missing, non-folder, foreign and unchanged rename targets atomically', () => {
		const state = setup();
		for (const folder of [own('missing'), own('playlist'), own('foreign-only'), foreign('root')]) {
			expect(updatePlaylistOrganizer(state, {
				type: 'rename-folder', folder, name: 'Renamed'
			})).toBe(state);
		}
		expect(updatePlaylistOrganizer(state, {
			type: 'rename-folder', folder: own('root'), name: '  Collection  '
		})).toBe(state);
		expect(state.items).toEqual(items);
	});

	it('refuses to delete folders with folder or playlist children and refuses playlists', () => {
		const state = setup();
		for (const folder of [own('root'), own('nested'), own('playlist'), own('missing'), own('foreign-only')]) {
			expect(updatePlaylistOrganizer(state, { type: 'delete-folder', folder })).toBe(state);
		}
		expect(state.items).toEqual(items);
	});

	it('deletes only an owned empty folder and preserves surviving rows and the private sheet', () => {
		const moved = updatePlaylistOrganizer(setup(), {
			type: 'move', item: own('playlist'), destination: null
		});
		const state = updatePlaylistOrganizer(moved, {
			type: 'open-share-sheet', playlist: own('playlist')
		});
		expect(updatePlaylistOrganizer(state, { type: 'delete-folder', folder: foreign('nested') })).toBe(state);
		const action = { type: 'delete-folder', folder: own('nested') } as const;
		const deleted = updatePlaylistOrganizer(state, action);

		expect(deleted.items).toEqual(state.items.slice(1));
		for (const [index, item] of deleted.items.entries()) expect(item).toBe(state.items[index + 1]);
		expect(deleted.shareSheet).toBe(state.shareSheet);
		expect(state.items).toEqual([items[0], { ...items[1], parentId: null }, ...items.slice(2)]);
		expect(updatePlaylistOrganizer(deleted, action)).toBe(deleted);
	});
});
