import { afterEach, describe, expect, it, vi } from 'vitest';
import { createM3UDownload, m3uDownloadFailure } from './m3uDownload.js';

function browser() {
	const events: string[] = [];
	const links: { href: string; download: string; hidden: boolean; click: () => void; remove: () => void }[] = [];
	const blobs: Blob[] = [];
	const createElement = vi.fn(() => {
		const link = {
			href: '', download: '', hidden: false,
			click: vi.fn(() => { events.push('click'); }),
			remove: vi.fn(() => { events.push('remove'); })
		};
		links.push(link);
		return link;
	});
	const append = vi.fn(() => { events.push('append'); });
	vi.stubGlobal('document', { createElement, body: { append } });
	const create = vi.spyOn(URL, 'createObjectURL').mockImplementation((blob) => {
		blobs.push(blob as Blob);
		return `blob:download-${blobs.length}`;
	});
	const revoke = vi.spyOn(URL, 'revokeObjectURL').mockImplementation((url) => { events.push(`revoke:${url}`); });
	return { events, links, blobs, createElement, append, create, revoke };
}

afterEach(() => {
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

describe('per-owner M3U download', () => {
	it('can be constructed and released during SSR without DOM or URL work', () => {
		vi.stubGlobal('document', undefined);
		vi.stubGlobal('URL', undefined);
		const file = createM3UDownload();
		expect(() => { file.release(); file.release(); }).not.toThrow();
	});

	it('sends exact Unicode bytes and MIME type through a temporary hidden native link', async () => {
		const env = browser();
		const file = createM3UDownload();
		const text = '#EXTM3U\n#EXTINF:-1,Björk — 日本語\ntrack-a';
		expect(file.download(() => text, 'selected.m3u')).toBe('Download started. Check your browser’s downloads for selected.m3u.');
		expect(env.createElement).toHaveBeenCalledWith('a');
		expect(await env.blobs[0].text()).toBe(text);
		expect(env.blobs[0].type).toBe('audio/x-mpegurl;charset=utf-8');
		expect(env.links[0]).toMatchObject({ href: 'blob:download-1', download: 'selected.m3u', hidden: true });
		expect(env.append).toHaveBeenCalledWith(env.links[0]);
		expect(env.events).toEqual(['append', 'click', 'remove']);
		expect(env.revoke).not.toHaveBeenCalled();
		file.release();
	});

	it('releases the preceding URL before evaluating the next payload, retaining only the new URL', () => {
		const env = browser();
		const file = createM3UDownload();
		file.download(() => 'first', 'queue.m3u');
		file.download(() => {
			expect(env.revoke).toHaveBeenCalledExactlyOnceWith('blob:download-1');
			return 'second';
		}, 'queue.m3u');
		expect(env.links[1].href).toBe('blob:download-2');
		expect(env.revoke).not.toHaveBeenCalledWith('blob:download-2');
		file.release();
		file.release();
		expect(env.revoke.mock.calls).toEqual([['blob:download-1'], ['blob:download-2']]);
	});

	it('never revokes another owner’s retained URL', () => {
		const env = browser();
		const queue = createM3UDownload();
		const favorites = createM3UDownload();
		queue.download(() => 'queue', 'queue.m3u');
		favorites.download(() => 'favorites', 'favorites.m3u');
		queue.release();
		expect(env.revoke.mock.calls).toEqual([['blob:download-1']]);
		favorites.release();
		expect(env.revoke.mock.calls).toEqual([['blob:download-1'], ['blob:download-2']]);
	});

	it.each(['content', 'url', 'append', 'click', 'remove'] as const)('propagates %s refusal, removes its link and allows owner cleanup', (phase) => {
		const env = browser();
		const file = createM3UDownload();
		const error = new Error(phase);
		const fail = () => { throw error; };
		if (phase === 'url') env.create.mockImplementation(fail);
		if (phase === 'append') env.append.mockImplementation(fail);
		if (phase === 'click' || phase === 'remove') {
			env.createElement.mockImplementation(() => {
				const link = { href: '', download: '', hidden: false, click: vi.fn(), remove: vi.fn() };
				link[phase].mockImplementation(fail);
				env.links.push(link);
				return link;
			});
		}
		expect(() => file.download(phase === 'content' ? fail : () => 'text', 'queue.m3u')).toThrow(error);
		expect(env.links[0].remove).toHaveBeenCalledOnce();
		file.release();
		expect(env.revoke.mock.calls).toEqual(['content', 'url'].includes(phase) ? [] : [['blob:download-1']]);
	});

	it('lets the owner release the previous URL when link creation itself fails', () => {
		const env = browser();
		const file = createM3UDownload();
		file.download(() => 'first', 'queue.m3u');
		env.createElement.mockImplementation(() => { throw new Error('No DOM'); });
		expect(() => file.download(() => 'second', 'queue.m3u')).toThrow('No DOM');
		file.release();
		expect(env.revoke).toHaveBeenCalledExactlyOnceWith('blob:download-1');
	});

	it.each([
		['queue.m3u', false, 'Download started. Check your browser’s downloads for queue.m3u.'],
		['selected.m3u', false, 'Download started. Check your browser’s downloads for selected.m3u.'],
		['queue.m3u', true, 'Download started. Check your browser’s saved downloads for queue.m3u.'],
		['favorites.m3u', true, 'Download started. Check your browser’s saved downloads for favorites.m3u.']
	] as const)('preserves the existing initiation-only feedback for %s (%s)', (filename, saved, message) => {
		browser();
		const file = createM3UDownload();
		expect(file.download(() => '#EXTM3U', filename, saved)).toBe(message);
		file.release();
	});

	it.each(['queue', 'favorites', 'selected tracks'])('preserves the existing %s refusal copy', (subject) => {
		expect(m3uDownloadFailure(subject)).toBe(`Could not start the download. Try again or copy the ${subject} as text.`);
	});
});
