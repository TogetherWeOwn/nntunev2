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
