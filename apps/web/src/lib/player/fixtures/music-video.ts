import type { VerifiedVideoLink } from '../musicVideo.js';

// Synthetic result from a successful batch check. The fixture build may
// use it without a catalogue, provider credentials or live validation.
export const fixtureLink: VerifiedVideoLink = {
	recordingId: 'cfa3bd0c-52a8-4b00-8f28-993d00001601',
	videoId: 'fixture0001',
	verifiedAt: Date.now()
};
