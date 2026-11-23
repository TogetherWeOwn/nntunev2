import { expect, it } from 'vitest';
import { verifiedVideoId, type VerifiedVideoLink } from './musicVideo.js';

it('accepts a fresh verified ID only for its own recording', () => {
	const now = Date.UTC(2026, 10, 23);
	const link: VerifiedVideoLink = { recordingId: 'first', videoId: 'fixture0001', verifiedAt: now - 1 };
	expect(verifiedVideoId(link, 'first', now)).toBe(link.videoId);
	expect(verifiedVideoId(link, 'second', now)).toBeNull();
	expect(verifiedVideoId({ ...link, verifiedAt: now - 30 * 86400_000 }, 'first', now)).toBeNull();
	expect(verifiedVideoId({ ...link, verifiedAt: now + 1 }, 'first', now)).toBeNull();
	expect(verifiedVideoId({ ...link, verifiedAt: NaN }, 'first', now)).toBeNull();
	for (const videoId of ['too-short', 'fixture0001\n', 'fixture000/', 'https://example.com/embed/fixture0001']) {
		expect(verifiedVideoId({ ...link, videoId }, 'first', now), videoId).toBeNull();
	}
});
