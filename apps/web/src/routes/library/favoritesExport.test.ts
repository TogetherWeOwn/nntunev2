import { afterEach, describe, expect, it, vi } from 'vitest';
import { createQueue, type Track } from '#lib/player/queue.js';
import { createFavoritesExport } from './favoritesExport.js';

function setup() {
	let tracks: Track[] = [{ id: 'one', title: 'One', artist: 'Artist', streamUrl: '' }];
	let feedback: [string, string, string?] | null = null;
	const exporter = createFavoritesExport({
		isEmpty: () => tracks.length === 0,
		snapshot: () => createQueue(tracks),
		currentKey: () => tracks.map((track) => `${track.id}\0${track.title}\0${track.artist}`).join('\u0001'),
		clearFeedback: () => { feedback = null; },
		setFeedback: (key, status, fallback) => { feedback = [key, status, fallback]; }
	});
	return { exporter, feedback: () => feedback, setTracks: (value: Track[]) => { tracks = value; } };
}

function browser() {
	const events: string[] = [];
	let count = 0;
	const link = { href: '', download: '', hidden: false, click: vi.fn(() => events.push('click')), remove: vi.fn(() => events.push('remove')) };
	const createElement = vi.fn(() => link);
	vi.stubGlobal('document', { createElement, body: { append: vi.fn(() => events.push('append')) } });
	vi.spyOn(URL, 'createObjectURL').mockImplementation(() => `blob:${++count}`);
	vi.spyOn(URL, 'revokeObjectURL').mockImplementation((url) => { events.push(`revoke:${url}`); });
	return { events, link, createElement };
}

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe('favorites export lifecycle', () => {
	it('refuses unsafe IDs before touching clipboard or download', async () => {
		const env = browser();
		const writeText = vi.fn();
		vi.stubGlobal('navigator', { clipboard: { writeText } });
		const state = setup();
		state.setTracks([{ id: 'bad/id', title: 'Bad', artist: '', streamUrl: '' }]);
		await state.exporter.copy();
		expect(state.feedback()?.[1]).toBe('Could not export favorites: a favorite has an unsafe track id.');
		state.exporter.downloadM3U();
		expect(state.feedback()?.[1]).toBe('Could not export favorites: a favorite has an unsafe track id.');
		expect(writeText).not.toHaveBeenCalled();
		expect(env.createElement).not.toHaveBeenCalled();
	});

	it('catches a throwing clipboard getter and a rejected write with the exact fallback', async () => {
		const state = setup();
		vi.stubGlobal('navigator', { get clipboard() { throw new Error('denied'); } });
		await state.exporter.copy();
		expect(state.feedback()?.slice(1)).toEqual(['Clipboard unavailable. Copy the favorites manually below.', '1. One — Artist']);
		vi.stubGlobal('navigator', { clipboard: { writeText: vi.fn().mockRejectedValue(new Error('denied')) } });
		await state.exporter.copy();
		expect(state.feedback()?.slice(1)).toEqual(['Clipboard unavailable. Copy the favorites manually below.', '1. One — Artist']);
	});

	it.each(['clear', 'changed', 'destroy'] as const)('fences a pending clipboard result after %s', async (change) => {
		let resolve!: () => void;
		vi.stubGlobal('navigator', { clipboard: { writeText: () => new Promise<void>((done) => { resolve = done; }) } });
		const state = setup();
		const pending = state.exporter.copy();
		if (change === 'clear') state.exporter.clear();
		if (change === 'changed') state.setTracks([{ id: 'two', title: 'Two', artist: '', streamUrl: '' }]);
		if (change === 'destroy') state.exporter.destroy();
		resolve();
		await pending;
		expect(state.feedback()).toBeNull();
	});

	it('retains and releases one URL in order; link creation remains outside the catch', () => {
		const env = browser();
		const state = setup();
		state.exporter.downloadM3U();
		expect(env.link).toMatchObject({ href: 'blob:1', download: 'favorites.m3u', hidden: true });
		expect(state.feedback()?.[1]).toBe('Download started. Check your browser’s saved downloads for favorites.m3u.');
		state.exporter.downloadM3U();
		expect(env.events).toEqual(['append', 'click', 'remove', 'revoke:blob:1', 'append', 'click', 'remove']);
		env.createElement.mockImplementation(() => { throw new Error('no anchor'); });
		expect(() => state.exporter.downloadM3U()).toThrow('no anchor');
		expect(state.feedback()).toBeNull();
		state.exporter.destroy();
		expect(env.events.at(-1)).toBe('revoke:blob:2');
	});

	it('cleans up before showing failure and removes the link afterwards', () => {
		const env = browser();
		const state = setup();
		env.link.click.mockImplementation(() => { throw new Error('denied'); });
		state.exporter.downloadM3U();
		expect(env.events).toEqual(['append', 'revoke:blob:1', 'remove']);
		expect(state.feedback()?.[1]).toBe('Could not start the download. Try again or copy the favorites as text.');
	});
});
