import { expect, it, vi } from 'vitest';
import { loadYouTubeApi, verifiedVideoId, type VerifiedVideoLink } from './musicVideo.js';

it('accepts a fresh verified ID only for its own recording', () => {
	const now = Date.UTC(2026, 10, 23);
	const link: VerifiedVideoLink = { recordingId: 'first', videoId: 'fixture0001', verifiedAt: now - 1 };
	expect(verifiedVideoId(link, 'first', now)).toBe(link.videoId);
	expect(verifiedVideoId(link, 'second', now)).toBeNull();
	expect(verifiedVideoId({ ...link, verifiedAt: now - 30 * 86400_000 }, 'first', now)).toBeNull();
	expect(verifiedVideoId({ ...link, verifiedAt: now + 1 }, 'first', now)).toBeNull();
	expect(verifiedVideoId({ ...link, verifiedAt: NaN }, 'first', now)).toBeNull();
	expect(verifiedVideoId({ ...link, verifiedAt: String(now) as unknown as number }, 'first', now)).toBeNull();
	expect(verifiedVideoId({ ...link, videoId: null as unknown as string }, 'first', now)).toBeNull();
	for (const videoId of ['too-short', 'fixture0001\n', 'fixture000/', 'https://example.com/embed/fixture0001']) {
		expect(verifiedVideoId({ ...link, videoId }, 'first', now), videoId).toBeNull();
	}
});

it('cleans up a failed script insertion and shares a single retry', async () => {
	vi.useFakeTimers();
	const script = () => ({ src: '', onerror: null, remove: vi.fn() });
	const first = script(), second = script();
	const host = { setTimeout, clearTimeout, YT: undefined as object | undefined,
		onYouTubeIframeAPIReady: undefined as (() => void) | undefined };
	let stale: (() => void) | undefined;
	const append = vi.fn().mockImplementationOnce(() => {
		stale = host.onYouTubeIframeAPIReady;
		throw new Error('blocked');
	});
	vi.stubGlobal('window', host);
	vi.stubGlobal('document', { head: { append }, createElement: vi.fn().mockReturnValueOnce(first).mockReturnValueOnce(second) });
	try {
		await expect(loadYouTubeApi()).rejects.toThrow();
		expect(first.remove).toHaveBeenCalledOnce();
		expect(host.onYouTubeIframeAPIReady).toBeUndefined();
		expect(vi.getTimerCount()).toBe(0);
		const pending = loadYouTubeApi();
		expect(loadYouTubeApi()).toBe(pending);
		await Promise.resolve();
		stale?.();
		host.YT = { Player: class {} };
		host.onYouTubeIframeAPIReady?.();
		await expect(pending).resolves.toBe(host.YT);
		expect(append).toHaveBeenCalledTimes(2);
		expect(vi.getTimerCount()).toBe(0);
	} finally {
		vi.unstubAllGlobals();
		vi.useRealTimers();
	}
});
