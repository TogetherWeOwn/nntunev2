import { afterEach, expect, it, vi } from 'vitest';
import { createPlayerSession } from './session.js';
import { createQueue, currentTrack } from './queue.js';
import { queueViewActions } from './queueViewActions.js';

function setup(index = 1) {
	const player = createPlayerSession({ flush: () => Promise.resolve() });
	player.start(null);
	player.setQueue({ ...createQueue(['a', 'b', 'c', 'd'].map((id) => ({ id, title: id.toUpperCase(), artist: '', streamUrl: '' }))), currentIndex: index });
	player.setQueueFilter('D');
	player.initializeFavorites();
	player.toggleFavorite('a');
	return { player, actions: queueViewActions(player) };
}

afterEach(() => vi.unstubAllGlobals());

it.each([
	['move', (a: ReturnType<typeof queueViewActions>) => a.onmove(3, 0), ['d', 'a', 'b', 'c'], 'b', 'D moved to 1 of 4'],
	['remove', (a: ReturnType<typeof queueViewActions>) => a.onremove(0), ['b', 'c', 'd'], 'b', 'A removed from the queue.'],
	['select', (a: ReturnType<typeof queueViewActions>) => a.onselect(3), ['a', 'b', 'c', 'd'], 'd', 'Selected D'],
	['clear', (a: ReturnType<typeof queueViewActions>) => a.onclear(), [], undefined, 'Queue cleared.'],
	['trim previous', (a: ReturnType<typeof queueViewActions>) => a.onkeepcurrentandupcoming(), ['b', 'c', 'd'], 'b', 'Removed 1 track before the current track.'],
	['trim upcoming', (a: ReturnType<typeof queueViewActions>) => a.onclearupcoming(), ['a', 'b'], 'b', 'Removed 2 upcoming tracks.'],
	['play next', (a: ReturnType<typeof queueViewActions>) => a.onplaynext(3), ['a', 'b', 'd', 'c'], 'b', '']
] as const)('%s reports full-queue coordinates to the real session', (_label, run, ids, selected, feedback) => {
	const { player, actions } = setup();
	run(actions);
	const state = player.snapshot();
	expect(state.queue.tracks.map((track) => track.id)).toEqual(ids);
	expect(currentTrack(state.queue)?.id).toBe(selected);
	expect(state.queueFeedback).toBe(feedback);
	expect(state.queueFilterQuery).toBe('D');
	expect(state.favorites).toEqual(['a']);
	player.destroy();
});

it.each([
	(a: ReturnType<typeof queueViewActions>) => a.onmove(-1, 2),
	(a: ReturnType<typeof queueViewActions>) => a.onremove(NaN),
	(a: ReturnType<typeof queueViewActions>) => a.onselect(1)
])('a no-op edit leaves queue identity and feedback intact', (run) => {
	const { player, actions } = setup();
	player.setQueueFeedback('Keep this outcome.');
	const before = player.snapshot();
	run(actions);
	expect(player.snapshot().queue).toBe(before.queue);
	expect(player.snapshot().queueFeedback).toBe(before.queueFeedback);
	expect(player.snapshot().queueFeedbackVersion).toBe(before.queueFeedbackVersion);
	player.destroy();
});

it('a play-next no-op keeps the queue but retires old feedback', () => {
	const { player, actions } = setup();
	player.setQueueFeedback('Old outcome.');
	const queue = player.snapshot().queue;
	actions.onplaynext(2);
	expect(player.snapshot().queue).toBe(queue);
	expect(player.snapshot().queueFeedback).toBe('');
	player.destroy();
});

it('queue-only Next uses the persistent selection path and stays clamped', async () => {
	const { player, actions } = setup(3);
	const before = player.snapshot().queue;
	actions.onnext();
	await Promise.resolve();
	expect(player.snapshot().queue).toBe(before);
	player.setQueue({ ...before, currentIndex: 1 });
	actions.onnext();
	await Promise.resolve();
	expect(currentTrack(player.snapshot().queue)?.id).toBe('c');
	player.destroy();
});

it('Shuffle delegates upcoming policy without moving history or the current selection', () => {
	const { player, actions } = setup();
	vi.spyOn(Math, 'random').mockReturnValue(0);
	try {
		actions.onshuffle();
		expect(player.snapshot().queue.tracks.map((track) => track.id)).toEqual(['a', 'b', 'd', 'c']);
		expect(currentTrack(player.snapshot().queue)?.id).toBe('b');
		expect(player.snapshot().queueFilterQuery).toBe('D');
	} finally {
		vi.restoreAllMocks();
		player.destroy();
	}
});

it('Copy uses the session clipboard controller without selecting the row', async () => {
	const { player, actions } = setup();
	const writeText = vi.fn(async () => {});
	vi.stubGlobal('window', { location: { origin: 'https://app.test' } });
	vi.stubGlobal('navigator', { clipboard: { writeText } });
	const queue = player.snapshot().queue;
	actions.oncopylink('d');
	await Promise.resolve();
	expect(writeText).toHaveBeenCalledExactlyOnceWith('https://app.test/library?track=d&t=0');
	expect(player.snapshot().queueShareStatus).toBe('Copied link for D.');
	expect(player.snapshot().queue).toBe(queue);
	player.destroy();
});

it('insertion dispatches current IDs and paired metadata/source input through the mounted producer only', async () => {
	const { player, actions } = setup();
	const received = vi.fn((ids: readonly string[], beforeId: string | null) => {
		const tracks = ids.map((id) => ({ id, title: id.toUpperCase(), artist: '', streamUrl: '' }));
		player.insertQueuedTracks(tracks, beforeId, { owner: player.sourceInputOwner(),
			tracks: ids.map((id) => ({ id, streamUrl: `/source/${id}` })) });
	});
	const stop = player.onQueueInsert(received);
	actions.oninsert(['x', 'x'], 'c');
	expect(received).toHaveBeenCalledExactlyOnceWith(['x'], 'c');
	expect(player.snapshot().queue.tracks.map((track) => track.id)).toEqual(['a', 'b', 'x', 'c', 'd']);
	expect(currentTrack(player.snapshot().queue)?.id).toBe('b');
	expect(player.snapshot().queue.tracks.find((track) => track.id === 'x')?.streamUrl).toBe('');
	expect(player.snapshot().queueFeedback).toBe('Added 1 to the queue.');
	stop();
	actions.oninsert(['y'], null);
	expect(received).toHaveBeenCalledTimes(1);
	actions.onplaynow('x');
	await Promise.resolve();
	expect(currentTrack(player.snapshot().queue)?.streamUrl).toBe('/source/x');
	expect(player.snapshot().queueFeedback).toBe('Selected X, paused: playback was blocked.');
	player.destroy();
});

it.each([
	['reorder', (a: ReturnType<typeof queueViewActions>) => a.onreorder(['d', 'c'], 'a'), ['c', 'd', 'a', 'b'], 'Queue order updated.'],
	['play next IDs', (a: ReturnType<typeof queueViewActions>) => a.onplaynextids(['d']), ['a', 'b', 'd', 'c'], 'Tracks placed next in the queue.'],
	['remove IDs', (a: ReturnType<typeof queueViewActions>) => a.onremoveids(['a']), ['b', 'c', 'd'], '1 selected track removed from queue.']
] as const)('%s uses explicit live operands without replacing hidden selection', (_label, run, ids, feedback) => {
	const { player, actions } = setup();
	player.setQueueFilter('');
	player.toggleQueueSelection('c');
	player.setQueueFilter('D');
	const selection = player.snapshot().queueSelection;
	const seen: string[][] = [];
	const stop = player.subscribe((snapshot) => { seen.push([...snapshot.queueSelection.ids]); });
	run(actions);
	expect(player.snapshot().queue.tracks.map((track) => track.id)).toEqual(ids);
	expect(currentTrack(player.snapshot().queue)?.id).toBe('b');
	expect(player.snapshot().queueSelection).toEqual(selection);
	expect(seen.every((ids) => ids.join(',') === 'c')).toBe(true);
	expect(player.snapshot().queueFeedback).toBe(feedback);
	stop();
	player.destroy();
});

it('the actual Favorites callback toggles one authoritative membership shared with other surfaces', () => {
	const { player, actions } = setup();
	actions.onfavoriteids(['a', 'b']);
	expect(player.snapshot().favorites).toEqual(['a', 'b']);
	expect(player.snapshot().queueFeedback).toBe('Starred 1 track; 1 was already starred.');
	actions.onfavoriteids(['a', 'b']);
	expect(player.snapshot().favorites).toEqual([]);
	expect(player.snapshot().queueFeedback).toBe('Removed 2 tracks from favorites.');
	player.destroy();
});

it.each(['loading', 'destroyed'] as const)('all six incoming callbacks reject a %s action without dispatch or revival', async (boundary) => {
	const { player, actions } = setup();
	const producer = vi.fn();
	player.onQueueInsert(producer);
	if (boundary === 'loading') player.setQueueLiveRegions(false);
	else player.destroy();
	const before = player.snapshot();
	actions.oninsert(['x'], null);
	actions.onreorder(['d'], 'a');
	actions.onplaynextids(['d']);
	actions.onremoveids(['a']);
	actions.onplaynow('a');
	actions.onfavoriteids(['a']);
	await Promise.resolve();
	expect(producer).not.toHaveBeenCalled();
	expect(player.snapshot()).toBe(before);
	player.destroy();
});

it('invalid insertion and reorder targets never become an end-of-queue fallback', () => {
	const { player, actions } = setup();
	const producer = vi.fn();
	player.onQueueInsert(producer);
	const before = player.snapshot();
	actions.oninsert(['x'], 'removed');
	actions.onreorder(['d'], 'removed');
	expect(producer).not.toHaveBeenCalled();
	expect(player.snapshot()).toBe(before);
	player.destroy();
});

it('old view callbacks cannot revive a destroyed owner', () => {
	const { player, actions } = setup();
	player.destroy();
	actions.onmove(0, 1);
	actions.onremove(0);
	actions.onselect(0);
	actions.onplaynext(0);
	actions.onshuffle();
	actions.onclear();
	actions.onkeepcurrentandupcoming();
	actions.onclearupcoming();
	actions.oncopylink('a');
	actions.onnext();
	expect(player.snapshot().queue.tracks).toEqual([]);
	expect(player.snapshot().queueFeedback).toBe('');
});
