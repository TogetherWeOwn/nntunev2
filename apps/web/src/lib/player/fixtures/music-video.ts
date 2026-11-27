import type { VerifiedVideoLink } from '../musicVideo.js';

// A synthetic, current verification receipt for the offline fixture build.
export const fixtureLink: Readonly<VerifiedVideoLink> = Object.freeze({
	recordingId: 'cfa3bd0c-52a8-4b00-8f28-993d00001601',
	videoId: 'fixture0001',
	verifiedAt: Date.now()
});
