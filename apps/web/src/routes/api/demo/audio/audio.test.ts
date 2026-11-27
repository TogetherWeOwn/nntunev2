import { describe, expect, it } from 'vitest';

import { GET } from './+server.js';
import { MEDIA_BYTES } from '#lib/player/demoMedia.js';

async function getAudio(range?: string, track?: string): Promise<Response> {
	const url = new URL('https://example.test/api/demo/audio');
	if (track !== undefined) url.searchParams.set('track', track);
	const request = new Request(url, {
		headers: range ? { range } : undefined
	});
	return GET({ request, url } as Parameters<typeof GET>[0]);
}

async function sha256(body: ArrayBuffer): Promise<string> {
	const digest = await crypto.subtle.digest('SHA-256', body);
	return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('');
}

describe('demo audio route', () => {
	it('returns the full body with 200 when no range is requested', async () => {
		const response = await getAudio();

		expect(response.status).toBe(200);
		expect(response.headers.get('content-type')).toBe('audio/wav');
		expect(response.headers.get('cache-control')).toMatch(/^private/);
		expect(response.headers.get('cache-control')).not.toContain('immutable');
		expect(response.headers.get('content-range')).toBeNull();
		expect(response.headers.get('content-length')).toBe(String(MEDIA_BYTES));
		expect((await response.arrayBuffer()).byteLength).toBe(MEDIA_BYTES);
	});

	it('returns a partial response for a satisfiable range', async () => {
		const response = await getAudio('bytes=0-99');

		expect(response.status).toBe(206);
		expect(response.headers.get('content-range')).toBe(`bytes 0-99/${MEDIA_BYTES}`);
		const body = await response.arrayBuffer();
		expect(body.byteLength).toBe(100);
		const decoder = new TextDecoder();
		expect(decoder.decode(body.slice(0, 4))).toBe('RIFF');
		expect(decoder.decode(body.slice(8, 12))).toBe('WAVE');
	});

	// Fingerprints of the independent WAV formula in e2e/helpers/demo-audio.ts.
	const defaultHash = 'c950476bd1f6006e970593a4e8e54af0878482159baff6418c5a3efd90aad729';
	it.each([
		{ track: undefined, hash: defaultHash },
		{ track: 'second-signal', hash: 'da636650f9823a6cdfa4a5de403e52d1a2e685471d8341d4c78afd2c26e3b49e' },
		{ track: 'third-light', hash: '8638ef5c2007ab4d6e9c9042e30cf8c96f033103472c2c750752ee5d6a0899ac' },
		{ track: 'no-such-track', hash: defaultHash }
	])('serves the exact synthetic WAV for track $track', async ({ track, hash }) => {
		const response = await getAudio(undefined, track);
		expect(response.status).toBe(200);
		expect(response.headers.get('content-type')).toBe('audio/wav');
		expect(await sha256(await response.arrayBuffer())).toBe(hash);
	});

	it('returns a partial response for a middle range', async () => {
		const response = await getAudio('bytes=1000-1099');

		expect(response.status).toBe(206);
		expect(response.headers.get('content-range')).toBe(`bytes 1000-1099/${MEDIA_BYTES}`);
		expect(response.headers.get('content-length')).toBe('100');
		const body = await response.arrayBuffer();
		expect(body.byteLength).toBe(100);
		// Offset 1000 starts at PCM sample 478 after the 44-byte header.
		expect(new DataView(body).getInt16(0, true)).toBe(16_975);
		expect(await sha256(body)).toBe(
			'c23409f029b09e7df618931f610c19c1dbf9fa5c9cc5d040c1411d09972e5c79'
		);
	});

	it('returns a partial response for an open-ended range', async () => {
		const response = await getAudio('bytes=0-');

		expect(response.status).toBe(206);
		expect(response.headers.get('content-range')).toBe(`bytes 0-${MEDIA_BYTES - 1}/${MEDIA_BYTES}`);
		expect((await response.arrayBuffer()).byteLength).toBe(MEDIA_BYTES);
	});

	it('clamps a range end past the body length', async () => {
		const response = await getAudio(`bytes=${MEDIA_BYTES - 144}-999999`);

		expect(response.status).toBe(206);
		expect(response.headers.get('content-range')).toBe(
			`bytes ${MEDIA_BYTES - 144}-${MEDIA_BYTES - 1}/${MEDIA_BYTES}`
		);
		expect((await response.arrayBuffer()).byteLength).toBe(144);
	});

	it('returns the trailing bytes for a suffix range', async () => {
		const response = await getAudio('bytes=-100');

		expect(response.status).toBe(206);
		expect(response.headers.get('content-range')).toBe(
			`bytes ${MEDIA_BYTES - 100}-${MEDIA_BYTES - 1}/${MEDIA_BYTES}`
		);
		expect((await response.arrayBuffer()).byteLength).toBe(100);
	});

	it('clamps an oversized suffix range to the full body', async () => {
		const response = await getAudio('bytes=-999999');

		expect(response.status).toBe(206);
		expect(response.headers.get('content-range')).toBe(`bytes 0-${MEDIA_BYTES - 1}/${MEDIA_BYTES}`);
		expect((await response.arrayBuffer()).byteLength).toBe(MEDIA_BYTES);
	});

	it('returns 416 with the complete length for an unsatisfiable range', async () => {
		const response = await getAudio(`bytes=${MEDIA_BYTES}-`);

		expect(response.status).toBe(416);
		expect(response.headers.get('content-range')).toBe(`bytes */${MEDIA_BYTES}`);
		expect(response.headers.get('content-length')).toBe('0');
		expect((await response.arrayBuffer()).byteLength).toBe(0);
	});

	it('returns 416 when the range starts past the body length', async () => {
		const response = await getAudio(`bytes=${MEDIA_BYTES + 100}-${MEDIA_BYTES + 200}`);

		expect(response.status).toBe(416);
		expect(response.headers.get('content-range')).toBe(`bytes */${MEDIA_BYTES}`);
		expect(response.headers.get('content-length')).toBe('0');
	});

	it('returns 416 when the range end precedes its start', async () => {
		const response = await getAudio('bytes=200-100');

		expect(response.status).toBe(416);
		expect(response.headers.get('content-range')).toBe(`bytes */${MEDIA_BYTES}`);
		expect(response.headers.get('content-length')).toBe('0');
		expect((await response.arrayBuffer()).byteLength).toBe(0);
	});

	it('returns 416 for a zero-length suffix range', async () => {
		const response = await getAudio('bytes=-0');

		expect(response.status).toBe(416);
		expect(response.headers.get('content-range')).toBe(`bytes */${MEDIA_BYTES}`);
		expect(response.headers.get('content-length')).toBe('0');
	});
});
