import { afterEach, expect, it, vi } from 'vitest';
import * as home from '../../routes/home/+page.js';
import * as library from '../../routes/library/+page.js';
import * as liked from '../../routes/library/liked/+page.js';
import * as recent from '../../routes/library/recent/+page.js';
import { ApiClient } from '../api/client.js';
import { isBuildShaKnown, parseBuildSha } from '../build.js';
import { designString } from '../design/strings.js';
import { listen, stopListening } from '../interactions/listen.js';
import type { LibraryItem } from './cache.js';
import { filterLibraryItems } from './filter.js';
import { groupLibraryItems } from './group.js';
import { createLibraryTrackLikes } from './library-track-likes.svelte.js';
import { createPlaylistOrganizer, updatePlaylistOrganizer } from './playlistOrganizer.js';
import type { LibraryRefreshUpdate } from './refresh.js';
import { createTrackLikeControls } from './track-like-controls.js';

const binding = 'A'.repeat(43);
const track = { id: 'Job_A', title: 'Café', artist: 'Constructor' };
const accountSession = {
	generation: 'test', signal: new AbortController().signal,
	isCurrent: () => true, run: async <T,>(work: () => T) => work()
};
const listing: LibraryRefreshUpdate = {
	authoritative: true, authBoundary: false, sessionBinding: binding, accountSession,
	isCurrent: accountSession.isCurrent, runIfCurrent: accountSession.run
};
const client = (fetchFn: typeof fetch) => new ApiClient({
	baseUrl: 'https://api.example.test', fetchFn, session: { owner: accountSession, binding }
});

afterEach(() => vi.unstubAllGlobals());

it('drops invalid rows while preserving literal IDs, canonical metadata and stable groups', () => {
	const fallback = { id: 'track[1]', artist: ' constructor ' };
	const unnamed = { id: 'unnamed', artist: 5 };
	const rows = [null, { id: 1 }, track, undefined, fallback, { id: '', title: 'Café' }, unnamed] as LibraryItem[];
	expect(filterLibraryItems(rows, '')).toEqual([track, fallback, unnamed]);
	expect(filterLibraryItems(rows, 'cafe\u0301')[0]).toBe(track);
	expect(filterLibraryItems(rows, '[1]')).toEqual([fallback]);
	expect(filterLibraryItems(rows, 'Job_A')).toEqual([]);
	expect(groupLibraryItems(rows)).toEqual([
		{ key: 'constructor', label: 'Constructor', items: [track, fallback] },
		{ key: null, label: 'Unknown artist', items: [unnamed] }
	]);
});

it('withholds conflicting Like targets without hiding verified rows', async () => {
	const mbid = 'abcd0000-0000-4000-8000-000000000001', alias = mbid.toUpperCase();
	const fetcher = vi.fn(async () => Response.json({ session_binding: binding, items: [] }));
	vi.stubGlobal('fetch', fetcher);
	const likes = createLibraryTrackLikes();
	await likes.publish(listing, [
		track, { ...track, occurrence_key: 'repeat' },
		{ id: alias, like_target_id: mbid }, { id: alias, like_target_id: alias }, { id: alias, like_target_id: mbid }
	]);
	expect(likes.controls(track.id)?.liked).toBe(false);
	expect(likes.controls(alias)).toBeUndefined();
	expect(fetcher).toHaveBeenCalledTimes(3);
});

it('refuses malformed Like state and stale or malformed write intents', async () => {
	const fetcher = vi.fn<typeof fetch>();
	const owner = {
		client: client(fetcher), binding, readiness: () => listing,
		liked: vi.fn<() => boolean>().mockReturnValue(false), confirm: vi.fn(), failed: vi.fn(),
		taste: { read: () => null, verify: async () => true, confirm: vi.fn(), invalidate: vi.fn() }
	};
	const controls = createTrackLikeControls(() => owner);
	const control = controls(track.id)!;
	expect(control.liked).toBe(false);
	await control.taste!.set('love' as never);
	owner.binding = 'E'.repeat(43);
	await control.toggle();
	expect(controls(track.id)).toBeUndefined();
	owner.binding = binding;
	owner.liked.mockReturnValue('false' as never);
	expect(controls(track.id)).toBeUndefined();
	owner.liked.mockReturnValue(false);
	expect(controls('../track')).toBeUndefined();
	expect(fetcher).not.toHaveBeenCalled();
});

it('requires affirmative verification before publishing a taste write', async () => {
	const fetcher = vi.fn(async () => Response.json({
		kind: 'track', target_id: track.id, signal: 'hide', until: null, session_binding: binding
	}));
	const taste = { read: () => null, verify: async () => 'true' as never, confirm: vi.fn(), invalidate: vi.fn() };
	const owner = { client: client(fetcher), binding, readiness: () => listing,
		liked: () => false, confirm: vi.fn(), failed: vi.fn(), taste };
	const controls = createTrackLikeControls(() => owner);
	await controls(track.id)!.taste!.set('hide');
	expect(fetcher).toHaveBeenCalledTimes(1);
	expect(taste.invalidate).toHaveBeenCalledTimes(1);
	expect(taste.confirm).not.toHaveBeenCalled();
});

it('rejects malformed organizer snapshots and actions atomically', () => {
	const folder = { ownerId: 'me', id: 'root', kind: 'folder', name: 'Root', parentId: null } as const;
	const playlist = { ...folder, id: 'playlist', kind: 'playlist', name: 'Playlist', parentId: folder.id } as const;
	const state = createPlaylistOrganizer('me', [folder, playlist])!;
	for (const row of [null, { ...folder, id: '' }, { ...folder, kind: 'other' }, { ...folder, name: 'Root\t' }]) {
		expect(createPlaylistOrganizer('me', [row])).toBeNull();
	}
	for (const action of [null, { type: 'unknown' }, { type: 'create-folder', folder: { ...folder, id: 'new' }, name: 'New' },
		{ type: 'rename-folder', folder, name: 5 }, { type: 'move', item: playlist, destination: false },
		{ type: 'open-share-sheet' }, { type: 'delete-folder', folder: { id: folder.id } }]) {
		expect(updatePlaylistOrganizer(state, action as never)).toBe(state);
	}
	expect(state.items).toEqual([folder, playlist]);
	const moved = updatePlaylistOrganizer(state, { type: 'move', item: playlist, destination: null });
	expect(moved.items[1].parentId).toBeNull();
});

it('finishes listener teardown even when an earlier cleanup fails', () => {
	const target = new EventTarget();
	const handler = vi.fn();
	const stop = listen(target, 'click', handler);
	target.dispatchEvent(new Event('click'));
	const failure = new Error('cleanup failed');
	expect(() => stopListening([() => { throw failure; }, stop])).toThrow(failure);
	target.dispatchEvent(new Event('click'));
	expect(handler).toHaveBeenCalledTimes(1);
});

it('returns only declared design strings and exact lowercase build identities', () => {
	expect(designString('design.icon.play')).toBe('Play');
	for (const key of ['constructor', '__proto__', 'design.icon.missing', ['design.icon.play']]) {
		expect(designString(key)).toBe('');
	}
	const sha = 'a1'.repeat(20);
	expect(parseBuildSha(sha)).toBe(sha);
	for (const value of [undefined, [sha], `${sha}\n`, ` ${sha}`, sha.toUpperCase()]) {
		expect(parseBuildSha(value)).toBe('unknown');
		expect(isBuildShaKnown(value)).toBe(false);
	}
});

it.each([home, library, liked, recent])('keeps private routes client-rendered without a loader', (route) => {
	expect({ ...route }).toEqual({ ssr: false });
});
