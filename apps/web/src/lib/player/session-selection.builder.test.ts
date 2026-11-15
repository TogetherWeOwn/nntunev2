/** Builder-owned tests for layout-local queue multi-selection state; no UI is implied. */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { createPlayerSession } from './session.js';
import { createQueue, type Track } from './queue.js';

const tracks: Track[] = [
	{ id: 'a', title: 'Alpha', artist: 'Artist', streamUrl: '/a' },
	{ id: 'b', title: 'Beta', artist: 'Artist', streamUrl: '/b' },
	{ id: 'c', title: 'Alpha Two', artist: 'Artist', streamUrl: '/c' },
	{ id: 'd', title: 'Delta', artist: 'Artist', streamUrl: '/d' },
	{ id: 'e', title: 'Alpha Three', artist: 'Artist', streamUrl: '/e' }
];

class AudioFake extends EventTarget {
	paused = true;
	currentTime = 0;
	duration = 100;
	volume = 1;
	muted = false;
	playbackRate = 1;
	readyState = 4;
	ended = false;
	error: MediaError | null = null;
	get seekable() { return { length: 1, start: () => 0, end: () => this.duration }; }
	constructor(public src: string) { super(); }
	play = vi.fn(async () => { this.paused = false; this.emit('play'); this.emit('playing'); });
	pause = vi.fn(() => { this.paused = true; this.emit('pause'); });
	load = vi.fn();
	getAttribute(name: string) { return name === 'src' ? this.src : null; }
	emit(name: string) { this.dispatchEvent(new Event(name)); }
	get element() { return this as unknown as HTMLAudioElement; }
}

const sessions: ReturnType<typeof createPlayerSession>[] = [];

function setup(list: readonly Track[] = tracks) {
	const pending: (() => void)[] = [];
	const flush = vi.fn(() => new Promise<void>((resolve) => { pending.push(resolve); }));
	const release = async () => {
		pending.splice(0).forEach((resolve) => resolve());
		await new Promise((resolve) => setTimeout(resolve, 0));
	};
	const storage = { getItem: vi.fn(() => null), setItem: vi.fn(), removeItem: vi.fn() };
	const session = createPlayerSession({ flush });
	sessions.push(session);
	session.start(storage);
	session.setQueue(createQueue(list));
	return { session, storage, flush, release };
}

const ids = (session: ReturnType<typeof setup>['session']) => session.snapshot().queueSelection.ids;
const EMPTY = { ids: [], anchorId: null, active: false };

afterEach(() => {
	for (const session of sessions.splice(0)) session.destroy();
	vi.unstubAllGlobals();
});

describe('queue selection session state', () => {
	it('starts empty and inactive, before and after start', () => {
		const bare = createPlayerSession({ flush: async () => {} });
		expect(bare.snapshot().queueSelection).toEqual(EMPTY);
		expect(setup().session.snapshot().queueSelection).toEqual(EMPTY);
	});

	it('publishes toggle, range, select all and clear through the snapshot and subscribers', () => {
		const { session } = setup();
		const seen: unknown[] = [];
		const stop = session.subscribe((s) => seen.push(s.queueSelection));
		seen.length = 0;
		session.toggleQueueSelection('b');
		expect(session.snapshot().queueSelection).toEqual({ ids: ['b'], anchorId: 'b', active: true });
		session.toggleQueueSelection('d', true);
		expect(ids(session)).toEqual(['b', 'c', 'd']);
		session.selectAllQueueRows();
		expect(ids(session)).toEqual(['b', 'c', 'd', 'a', 'e']);
		session.clearQueueSelection();
		expect(session.snapshot().queueSelection).toEqual(EMPTY);
		expect(seen).toHaveLength(4);
		stop();
	});

	it('keeps the mode active after the last row is toggled off until Done (clear)', () => {
		const { session } = setup();
		session.toggleQueueSelection('a');
		session.toggleQueueSelection('a');
		expect(session.snapshot().queueSelection).toEqual({ ids: [], anchorId: 'a', active: true });
		session.clearQueueSelection();
		expect(session.snapshot().queueSelection).toEqual(EMPTY);
	});

	it('uses the live filtered projection: range skips hidden rows and select all keeps hidden members', () => {
		const { session } = setup();
		session.toggleQueueSelection('b');
		session.setQueueFilter('alpha');
		expect(session.snapshot().queueSelection.ids).toEqual(['b']);
		session.toggleQueueSelection('a');
		session.toggleQueueSelection('e', true);
		expect(ids(session)).toEqual(['b', 'a', 'c', 'e']);
		session.setQueueFilter('delta');
		session.selectAllQueueRows();
		expect(ids(session)).toEqual(['b', 'a', 'c', 'e', 'd']);
	});

	it('drops a hidden anchor on filtering but keeps every selected id, and clearing the filter does not restore it', () => {
		const { session } = setup();
		session.toggleQueueSelection('b');
		session.setQueueFilter('alpha');
		expect(session.snapshot().queueSelection).toEqual({ ids: ['b'], anchorId: null, active: true });
		session.setQueueFilter('');
		expect(session.snapshot().queueSelection).toEqual({ ids: ['b'], anchorId: null, active: true });
		session.toggleQueueSelection('d', true);
		expect(ids(session)).toEqual(['b', 'd']);
	});

	it('ignores a toggle of a row the filter hides, without publishing', () => {
		const { session } = setup();
		session.setQueueFilter('alpha');
		const before = session.snapshot();
		const observer = vi.fn();
		const stop = session.subscribe(observer);
		observer.mockClear();
		session.toggleQueueSelection('b');
		session.toggleQueueSelection('b', true);
		session.toggleQueueSelection('missing');
		expect(observer).not.toHaveBeenCalled();
		expect(session.snapshot()).toBe(before);
		stop();
	});

	it('no-ops keep snapshot identity: repeated select all, clear when inactive, no displayed rows', () => {
		const { session } = setup();
		const initial = session.snapshot();
		session.clearQueueSelection();
		expect(session.snapshot()).toBe(initial);
		session.setQueueFilter('nothing matches');
		const filtered = session.snapshot();
		session.selectAllQueueRows();
		expect(session.snapshot()).toBe(filtered);
		session.setQueueFilter('');
		session.selectAllQueueRows();
		const selected = session.snapshot();
		session.selectAllQueueRows();
		expect(session.snapshot()).toBe(selected);
		// Range with no anchor degrades to a toggle, so it is a real change, not a no-op.
		session.toggleQueueSelection('a', true);
		expect(ids(session)).toEqual(['b', 'c', 'd', 'e']);
		session.clearQueueSelection();
		session.toggleQueueSelection('a');
		session.selectAllQueueRows();
		const anchored = session.snapshot();
		session.toggleQueueSelection('e', true);
		expect(session.snapshot()).toBe(anchored);
	});

	it('prunes selected ids only when absent from the whole queue, including ids hidden by the filter', () => {
		const { session } = setup();
		['a', 'b', 'd'].forEach((id) => session.toggleQueueSelection(id));
		session.setQueueFilter('alpha');
		expect(ids(session)).toEqual(['a', 'b', 'd']);
		session.editQueue({ type: 'remove', index: 1 });
		expect(ids(session)).toEqual(['a', 'd']);
		expect(session.snapshot().queueSelection.active).toBe(true);
	});

	it('removing the anchored row clears the anchor and keeps the mode while rows remain', () => {
		const { session } = setup();
		session.toggleQueueSelection('a');
		session.toggleQueueSelection('c');
		session.editQueue({ type: 'remove', index: 2 });
		expect(session.snapshot().queueSelection).toEqual({ ids: ['a'], anchorId: null, active: true });
	});

	it('clear queue empties the selection and ends the mode, and a refill does not resurrect it', () => {
		const { session } = setup();
		session.toggleQueueSelection('a');
		session.selectAllQueueRows();
		session.editQueue({ type: 'clear' });
		expect(session.snapshot().queueSelection).toEqual(EMPTY);
		session.setQueue(createQueue(tracks));
		expect(session.snapshot().queueSelection).toEqual(EMPTY);
	});

	it('trims prune only removed ids and leave surviving selections in activation order', () => {
		const { session } = setup();
		session.setQueue({ ...session.snapshot().queue, currentIndex: 2 });
		['e', 'a', 'c', 'd'].forEach((id) => session.toggleQueueSelection(id));
		session.editQueue({ type: 'trimPrevious' });
		expect(ids(session)).toEqual(['e', 'c', 'd']);
		session.editQueue({ type: 'trimUpcoming' });
		expect(ids(session)).toEqual(['c']);
		expect(session.snapshot().queueSelection.active).toBe(true);
	});

	it('keeps ids and mode across a reorder and ranges over the new arranged order', () => {
		const { session } = setup();
		session.toggleQueueSelection('a');
		session.editQueue({ type: 'move', from: 0, to: 3 });
		expect(session.snapshot().queue.tracks.map((t) => t.id)).toEqual(['b', 'c', 'd', 'a', 'e']);
		expect(session.snapshot().queueSelection).toEqual({ ids: ['a'], anchorId: 'a', active: true });
		session.toggleQueueSelection('c', true);
		expect(ids(session)).toEqual(['a', 'c', 'd']);
	});

	it('a replacement queue with the same ids but new object identity keeps the selection object', () => {
		const { session } = setup();
		session.toggleQueueSelection('a');
		const selection = session.snapshot().queueSelection;
		session.setQueue(createQueue(tracks.map((t) => ({ ...t }))));
		expect(session.snapshot().queueSelection).toBe(selection);
	});

	it('a stale id deleted while hidden stays gone when it reappears', () => {
		const { session } = setup();
		session.toggleQueueSelection('d');
		session.setQueueFilter('alpha');
		session.setQueue(createQueue(tracks.filter((t) => t.id !== 'd')));
		expect(ids(session)).toEqual([]);
		session.setQueue(createQueue(tracks));
		session.setQueueFilter('');
		expect(ids(session)).toEqual([]);
		expect(session.snapshot().queueSelection.active).toBe(true);
	});

	it.each(['resetAtAuthBoundary', 'destroy'] as const)('%s clears ids, anchor and mode', (boundary) => {
		const { session } = setup();
		session.toggleQueueSelection('a');
		session.toggleQueueSelection('c', true);
		session[boundary]();
		expect(session.snapshot().queueSelection).toEqual(EMPTY);
	});

	it('the next account starts clean after an auth reset and can select again', () => {
		const { session } = setup();
		session.toggleQueueSelection('a');
		session.resetAtAuthBoundary();
		session.setQueue(createQueue(tracks));
		expect(session.snapshot().queueSelection).toEqual(EMPTY);
		session.toggleQueueSelection('b');
		expect(ids(session)).toEqual(['b']);
	});

	it('before start and after destroy, selection callbacks do not publish', () => {
		const session = createPlayerSession({ flush: async () => {} });
		session.setQueue(createQueue(tracks));
		for (const boundary of [() => {}, () => { session.start(null); session.destroy(); }]) {
			boundary();
			session.setQueue(createQueue(tracks));
			const before = session.snapshot();
			const observer = vi.fn();
			const stop = session.subscribe(observer);
			observer.mockClear();
			session.toggleQueueSelection('a');
			session.toggleQueueSelection('b', true);
			session.selectAllQueueRows();
			session.clearQueueSelection();
			expect(observer).not.toHaveBeenCalled();
			expect(session.snapshot()).toBe(before);
			expect(session.snapshot().queueSelection).toEqual(EMPTY);
			stop();
		}
	});

	it('a destroyed session ignores a late selection after a previously active mode', () => {
		const { session } = setup();
		session.toggleQueueSelection('a');
		session.destroy();
		session.toggleQueueSelection('a');
		expect(session.snapshot().queueSelection).toEqual(EMPTY);
	});

	it('separate layout instances never share selection', () => {
		const first = setup().session;
		const second = setup().session;
		first.toggleQueueSelection('a');
		first.selectAllQueueRows();
		expect(second.snapshot().queueSelection).toEqual(EMPTY);
		second.toggleQueueSelection('e');
		first.resetAtAuthBoundary();
		expect(ids(second)).toEqual(['e']);
		expect(first.snapshot().queueSelection).toEqual(EMPTY);
	});

	it('survives subscriber removal and remount with the same selection object', () => {
		const { session } = setup();
		const stop = session.subscribe(() => {});
		session.toggleQueueSelection('c');
		session.toggleQueueSelection('a');
		stop();
		session.toggleQueueSelection('d');
		let seen = null as unknown;
		const again = session.subscribe((s) => { seen = s.queueSelection; });
		expect(seen).toBe(session.snapshot().queueSelection);
		expect(seen).toEqual({ ids: ['c', 'a', 'd'], anchorId: 'd', active: true });
		expect(get(session).queueSelection).toBe(seen);
		again();
	});

	it('matches synthesized-title ids and canonical accents through the session projection', () => {
		const { session } = setup([
			{ id: 'needle-7', title: '', titleIsFallback: true, artist: '', streamUrl: '/n' },
			{ id: 'cafe', title: 'Café', artist: 'Artist', streamUrl: '/c' },
			{ id: 'other', title: 'Other', artist: 'Artist', streamUrl: '/o' }
		]);
		session.setQueueFilter('NEEDLE-7');
		session.selectAllQueueRows();
		expect(ids(session)).toEqual(['needle-7']);
		session.setQueueFilter('Café');
		session.selectAllQueueRows();
		expect(ids(session)).toEqual(['needle-7', 'cafe']);
	});
});

describe('selected queue batch edits', () => {
	const queueIds = (session: ReturnType<typeof setup>['session']) => session.snapshot().queue.tracks.map((t) => t.id);

	it('removes the whole selected subset atomically, including hidden members, and prunes its anchor', () => {
		const { session } = setup();
		['d', 'b'].forEach((id) => session.toggleQueueSelection(id));
		session.setQueueFilter('alpha');
		const seen: string[][] = [];
		const stop = session.subscribe((s) => { seen.push(s.queue.tracks.map((t) => t.id)); });
		seen.length = 0;
		expect(session.editQueueSelection('remove')).toBe(true);
		expect(queueIds(session)).toEqual(['a', 'c', 'e']);
		expect(session.snapshot().queueSelection).toEqual({ ids: [], anchorId: null, active: true });
		expect(seen.length).toBeGreaterThan(0);
		expect(seen.every((value) => value.join(',') === 'a,b,c,d,e' || value.join(',') === 'a,c,e')).toBe(true);
		stop();
	});

	it('moves selected rows next in arranged order, not activation order, without duplicating the current row', () => {
		const { session } = setup();
		session.setQueue({ ...session.snapshot().queue, currentIndex: 2 });
		['e', 'c', 'a'].forEach((id) => session.toggleQueueSelection(id));
		const selection = session.snapshot().queueSelection;
		session.setQueueFilter('delta');
		expect(session.editQueueSelection('playNext')).toBe(true);
		expect(queueIds(session)).toEqual(['b', 'c', 'a', 'e', 'd']);
		expect(session.snapshot().queue.currentIndex).toBe(1);
		expect(ids(session)).toEqual(selection.ids);
		expect(session.snapshot().queueFilterQuery).toBe('delta');
	});

	it.each(['remove', 'playNext'] as const)('%s no-op keeps feedback, consent, exports and subscribers untouched', async (action) => {
		const { session } = setup();
		if (action === 'playNext') session.toggleQueueSelection('a');
		session.setQueueFeedback('Kept.', true);
		vi.stubGlobal('navigator', { clipboard: { writeText: async () => {} } });
		await session.queueExport.copy();
		const before = session.snapshot();
		const exported = get(session.queueExport);
		const observer = vi.fn();
		const stop = session.subscribe(observer);
		observer.mockClear();
		expect(session.editQueueSelection(action)).toBe(false);
		expect(session.snapshot()).toBe(before);
		expect(get(session.queueExport)).toBe(exported);
		expect(observer).not.toHaveBeenCalled();
		stop();
	});

	it.each(['remove', 'playNext'] as const)('%s no-op does not retire pending Next playback intent', async (action) => {
		const { session, release } = setup();
		const first = new AudioFake('/a');
		session.attach(first.element);
		await session.play();
		const moving = session.next();
		const second = new AudioFake('/b');
		second.currentTime = 30;
		session.attach(second.element);
		if (action === 'playNext') session.toggleQueueSelection('c');
		expect(session.editQueueSelection(action)).toBe(false);
		await release();
		await moving;
		expect(second.currentTime).toBe(0);
		expect(second.play).toHaveBeenCalledTimes(1);
	});

	it.each(['remove', 'playNext'] as const)('%s preserves row-share settlement semantics for surviving and removed ids', async (action) => {
		for (const refused of [false, true]) {
			const { session } = setup();
			vi.stubGlobal('window', { location: { origin: 'https://example.test' } });
			let resolve!: () => void;
			let reject!: (error: Error) => void;
			const writeText = vi.fn(() => new Promise<void>((yes, no) => { resolve = yes; reject = no; }));
			vi.stubGlobal('navigator', { clipboard: { writeText } });
			const copying = session.copyQueuedTrackLink('d');
			expect(writeText).toHaveBeenCalledWith('https://example.test/library?track=d&t=0');
			session.toggleQueueSelection('d');
			expect(session.editQueueSelection(action)).toBe(true);
			if (refused) reject(new Error('Refused')); else resolve();
			await copying;
			expect(session.snapshot()).toMatchObject(action === 'remove'
				? { queueShareStatus: 'Link unavailable — Delta is no longer in the queue.', queueShareFallbackUrl: null }
				: refused ? { queueShareStatus: '', queueShareFallbackUrl: 'https://example.test/library?track=d&t=0' }
					: { queueShareStatus: 'Copied link for Delta.', queueShareFallbackUrl: null });
		}
	});

	it('removing every row ends selection mode; a later refill cannot resurrect it', () => {
		const { session } = setup();
		session.selectAllQueueRows();
		expect(session.editQueueSelection('remove')).toBe(true);
		expect(session.snapshot().queue).toEqual({ tracks: [], currentIndex: -1 });
		expect(session.snapshot().queueSelection).toEqual(EMPTY);
		session.setQueue(createQueue(tracks));
		expect(session.snapshot().queueSelection).toEqual(EMPTY);
	});

	it.each(['remove', 'playNext'] as const)('%s preserves live current media when it survives, with no seek/play/pause/load or fetch', async (action) => {
		const { session, flush } = setup();
		const audio = new AudioFake('/a');
		session.attach(audio.element);
		await session.play();
		audio.currentTime = 42;
		audio.play.mockClear();
		session.toggleQueueSelection('d');
		const fetchSpy = vi.fn();
		vi.stubGlobal('fetch', fetchSpy);
		expect(session.editQueueSelection(action)).toBe(true);
		expect(session.snapshot().audio).toBe(audio.element);
		expect(session.snapshot().transport?.status).toBe('playing');
		expect(audio.currentTime).toBe(42);
		expect(audio.play).not.toHaveBeenCalled();
		expect(audio.pause).not.toHaveBeenCalled();
		expect(audio.load).not.toHaveBeenCalled();
		expect(flush).not.toHaveBeenCalled();
		expect(fetchSpy).not.toHaveBeenCalled();
	});

	it('removing the current row disconnects it but never autoplays its successor', async () => {
		const { session, flush } = setup();
		const audio = new AudioFake('/a');
		session.attach(audio.element);
		await session.play();
		audio.play.mockClear();
		session.toggleQueueSelection('a');
		session.toggleQueueSelection('b');
		expect(session.editQueueSelection('remove')).toBe(true);
		expect(queueIds(session)).toEqual(['c', 'd', 'e']);
		expect(session.snapshot().queue.currentIndex).toBe(0);
		expect(session.snapshot().audio).toBeNull();
		expect(audio.pause).toHaveBeenCalledTimes(1);
		const successor = new AudioFake('/c');
		session.attach(successor.element);
		expect(successor.play).not.toHaveBeenCalled();
		expect(audio.play).not.toHaveBeenCalled();
		expect(flush).not.toHaveBeenCalled();
	});

	it.each(['remove', 'playNext'] as const)('%s retires an older pending Next continuation', async (action) => {
		const { session, release } = setup();
		const first = new AudioFake('/a');
		session.attach(first.element);
		await session.play();
		const moving = session.next();
		const second = new AudioFake('/b');
		second.currentTime = 30;
		session.attach(second.element);
		session.toggleQueueSelection('e');
		expect(session.editQueueSelection(action)).toBe(true);
		await release();
		await moving;
		expect(second.currentTime).toBe(30);
		expect(second.play).not.toHaveBeenCalled();
		expect(session.snapshot().audio).toBe(second.element);
	});

	it.each(['resetAtAuthBoundary', 'destroy', 'setQueue'] as const)('does not overwrite a reentrant %s from an export-reset subscriber', (boundary) => {
		for (const action of ['remove', 'playNext'] as const) {
			const { session } = setup();
			session.toggleQueueSelection('d');
			const replacement = createQueue([tracks[4]]);
			let armed = false;
			const stop = session.queueExport.subscribe(() => {
				if (!armed) return;
				armed = false;
				if (boundary === 'setQueue') session.setQueue(replacement); else session[boundary]();
			});
			armed = true;
			expect(session.editQueueSelection(action)).toBe(false);
			expect(session.snapshot().queue).toEqual(boundary === 'setQueue' ? replacement : createQueue([]));
			if (boundary !== 'setQueue') expect(session.snapshot().queueSelection).toEqual(EMPTY);
			stop();
		}
	});

	it.each(['resetAtAuthBoundary', 'destroy', 'setQueue'] as const)('does not overwrite a reentrant %s from a source-disconnect subscriber', async (boundary) => {
		const { session } = setup();
		const audio = new AudioFake('/a');
		session.attach(audio.element);
		await session.play();
		session.toggleQueueSelection('a');
		const before = session.snapshot().transport;
		const replacement = createQueue([tracks[4]]);
		let armed = false;
		const stop = session.subscribe((state) => {
			if (!armed || state.transport === before) return;
			armed = false;
			if (boundary === 'setQueue') session.setQueue(replacement); else session[boundary]();
		});
		armed = true;
		expect(session.editQueueSelection('remove')).toBe(false);
		expect(session.snapshot().queue).toEqual(boundary === 'setQueue' ? replacement : createQueue([]));
		expect(session.snapshot().audio).toBeNull();
		if (boundary !== 'setQueue') expect(session.snapshot().queueSelection).toEqual(EMPTY);
		stop();
	});

	it.each(['resetAtAuthBoundary', 'destroy', 'setQueue'] as const)('does not overwrite a reentrant %s during batch navigation notification', (boundary) => {
		const { session } = setup();
		session.toggleQueueSelection('d');
		const replacement = createQueue([tracks[4]]);
		session.onBeforeNavigate(() => { if (boundary === 'setQueue') session.setQueue(replacement); else session[boundary](); });
		expect(session.editQueueSelection('remove')).toBe(false);
		expect(session.snapshot().queue).toEqual(boundary === 'setQueue' ? replacement : createQueue([]));
	});

	it.each(['remove', 'playNext'] as const)('%s ignores selection captured before auth reset or disposal', (action) => {
		const { session } = setup();
		session.toggleQueueSelection('d');
		session.resetAtAuthBoundary();
		session.setQueue(createQueue(tracks));
		expect(session.editQueueSelection(action)).toBe(false);
		expect(queueIds(session)).toEqual(['a', 'b', 'c', 'd', 'e']);
		session.toggleQueueSelection('e');
		session.destroy();
		session.setQueue(createQueue(tracks));
		const before = session.snapshot();
		expect(session.editQueueSelection(action)).toBe(false);
		expect(session.snapshot()).toBe(before);
	});

	it.each(['remove', 'playNext'] as const)('%s invalidates pending whole-queue export, retaining the whole edited queue for later exports', async (action) => {
		const { session } = setup();
		let release!: () => void;
		const writeText = vi.fn(() => new Promise<void>((resolve) => { release = resolve; }));
		vi.stubGlobal('navigator', { clipboard: { writeText } });
		const copying = session.queueExport.copy();
		session.toggleQueueSelection('d');
		expect(session.editQueueSelection(action)).toBe(true);
		release();
		await copying;
		expect(get(session.queueExport)).toEqual({ status: '', fallback: null });
		writeText.mockImplementation(async () => {});
		await session.queueExport.copy();
		expect(writeText.mock.calls).toHaveLength(2);
		expect(writeText).toHaveBeenLastCalledWith(action === 'remove'
			? '1. Alpha — Artist\n2. Beta — Artist\n3. Alpha Two — Artist\n4. Alpha Three — Artist'
			: '1. Alpha — Artist\n2. Delta — Artist\n3. Beta — Artist\n4. Alpha Two — Artist\n5. Alpha Three — Artist');
	});
});

describe('queue selection is selection-only', () => {
	function gestures(session: ReturnType<typeof setup>['session']) {
		session.toggleQueueSelection('b');
		session.toggleQueueSelection('d', true);
		session.selectAllQueueRows();
		session.toggleQueueSelection('b');
		session.clearQueueSelection();
	}

	it('changes only queueSelection: queue, feedback, consent, share, export, favorites and transport stay put', () => {
		const { session, storage, flush } = setup();
		const fetchSpy = vi.fn();
		vi.stubGlobal('fetch', fetchSpy);
		session.setQueueFeedback('Kept outcome.', true);
		session.initializeFavorites();
		session.toggleFavorite('a');
		session.invalidateQueueConsent();
		storage.setItem.mockClear();
		storage.removeItem.mockClear();
		const before = session.snapshot();
		const exportBefore = get(session.queueExport);
		gestures(session);
		session.toggleQueueSelection('c');
		const after = session.snapshot();
		expect(after).toEqual({ ...before, queueSelection: after.queueSelection });
		expect(after.queue).toBe(before.queue);
		expect(after.favorites).toBe(before.favorites);
		expect(after.queueFeedback).toBe('Kept outcome.');
		expect(after.queueFeedbackVersion).toBe(before.queueFeedbackVersion);
		expect(after.queueConfirmGeneration).toBe(before.queueConfirmGeneration);
		expect(get(session.queueExport)).toEqual(exportBefore);
		expect(storage.setItem).not.toHaveBeenCalled();
		expect(storage.removeItem).not.toHaveBeenCalled();
		expect(flush).not.toHaveBeenCalled();
		expect(fetchSpy).not.toHaveBeenCalled();
	});

	it('does not play, pause, seek or reload a live element', async () => {
		const { session } = setup();
		const audio = new AudioFake('/a');
		session.attach(audio.element);
		await session.play();
		audio.currentTime = 42;
		audio.play.mockClear();
		gestures(session);
		expect(audio.play).not.toHaveBeenCalled();
		expect(audio.pause).not.toHaveBeenCalled();
		expect(audio.load).not.toHaveBeenCalled();
		expect(audio.currentTime).toBe(42);
		expect(session.snapshot().transport?.status).toBe('playing');
		expect(session.snapshot().queue.currentIndex).toBe(0);
	});

	it('does not invalidate a pending Next: the continuation still seeks to start and plays', async () => {
		const { session, release } = setup();
		const first = new AudioFake('/a');
		session.attach(first.element);
		await session.play();
		const moving = session.next();
		const second = new AudioFake('/b');
		second.currentTime = 30;
		session.attach(second.element);
		gestures(session);
		session.toggleQueueSelection('e');
		await release();
		await moving;
		expect(second.currentTime).toBe(0);
		expect(second.play).toHaveBeenCalledTimes(1);
		expect(session.snapshot().queue.currentIndex).toBe(1);
		expect(ids(session)).toEqual(['e']);
	});

	it('Next after selecting keeps the selection, which is already reconciled against the same queue', async () => {
		const { session, release } = setup();
		session.toggleQueueSelection('b');
		const moving = session.next();
		await release();
		await moving;
		expect(session.snapshot().queueSelection).toEqual({ ids: ['b'], anchorId: 'b', active: true });
	});

	it.each([false, true])('preserves completed share and export outcomes (clipboard refused: %s)', async (refused) => {
		const { session } = setup();
		vi.stubGlobal('window', { location: { origin: 'https://example.test' } });
		const writeText = vi.fn(async () => { if (refused) throw new Error('Refused'); });
		vi.stubGlobal('navigator', { clipboard: { writeText } });
		await session.copyQueuedTrackLink('b');
		await session.queueExport.copy();
		const share = { queueShareStatus: session.snapshot().queueShareStatus, queueShareFallbackUrl: session.snapshot().queueShareFallbackUrl };
		const exported = get(session.queueExport);
		expect(share).toEqual(refused
			? { queueShareStatus: '', queueShareFallbackUrl: 'https://example.test/library?track=b&t=0' }
			: { queueShareStatus: 'Copied link for Beta.', queueShareFallbackUrl: null });
		expect(exported.status).toBe(refused ? 'Clipboard unavailable. Copy the queue manually below.' : 'Copied queue as text.');
		expect(exported.fallback).toBe(refused
			? '1. Alpha — Artist\n2. Beta — Artist\n3. Alpha Two — Artist\n4. Delta — Artist\n5. Alpha Three — Artist' : null);
		gestures(session);
		expect(session.snapshot()).toMatchObject(share);
		expect(get(session.queueExport)).toBe(exported);
		expect(writeText).toHaveBeenCalledTimes(2);
		session.destroy();
	});

	it.each([false, true])('does not retire pending share or export completions (clipboard refused: %s)', async (refused) => {
		const { session } = setup();
		vi.stubGlobal('window', { location: { origin: 'https://example.test' } });
		const writes: { resolve: () => void; reject: (error: Error) => void }[] = [];
		const writeText = vi.fn(() => new Promise<void>((resolve, reject) => writes.push({ resolve, reject })));
		vi.stubGlobal('navigator', { clipboard: { writeText } });
		const sharing = session.copyQueuedTrackLink('b');
		const exporting = session.queueExport.copy();
		expect(writes).toHaveLength(2);
		gestures(session);
		for (const write of writes) {
			if (refused) write.reject(new Error('Refused')); else write.resolve();
		}
		await Promise.all([sharing, exporting]);
		expect(session.snapshot()).toMatchObject(refused
			? { queueShareStatus: '', queueShareFallbackUrl: 'https://example.test/library?track=b&t=0' }
			: { queueShareStatus: 'Copied link for Beta.', queueShareFallbackUrl: null });
		expect(get(session.queueExport)).toEqual(refused
			? { status: 'Clipboard unavailable. Copy the queue manually below.', fallback: '1. Alpha — Artist\n2. Beta — Artist\n3. Alpha Two — Artist\n4. Delta — Artist\n5. Alpha Three — Artist' }
			: { status: 'Copied queue as text.', fallback: null });
		expect(writeText).toHaveBeenCalledTimes(2);
		session.destroy();
	});
});
