import { describe, expect, it } from 'vitest';

import { GET } from './+server.js';
import { MEDIA_BYTES } from '#lib/player/demoMedia.js';

async function getAudio(range?: string): Promise<Response> {
	const request = new Request('https://example.test/api/demo/audio', {
		headers: range ? { range } : undefined
	});
	return GET({ request, url: new URL(request.url) } as Parameters<typeof GET>[0]);
}

describe('demo audio route', () => {
	it('returns the full body with 200 when no range is requested', async () => {
		const response = await getAudio();

		expect(response.status).toBe(200);
		expect(response.headers.get('content-range')).toBeNull();
		expect(response.headers.get('content-length')).toBe(String(MEDIA_BYTES));
		expect((await response.arrayBuffer()).byteLength).toBe(MEDIA_BYTES);
	});

	it('returns a partial response for a satisfiable range', async () => {
		const response = await getAudio('bytes=0-99');

		expect(response.status).toBe(206);
		expect(response.headers.get('content-range')).toBe(`bytes 0-99/${MEDIA_BYTES}`);
		expect((await response.arrayBuffer()).byteLength).toBe(100);
	});

	it('returns a partial response for a middle range', async () => {
		const response = await getAudio('bytes=1000-1099');

		expect(response.status).toBe(206);
		expect(response.headers.get('content-range')).toBe(`bytes 1000-1099/${MEDIA_BYTES}`);
		expect(response.headers.get('content-length')).toBe('100');
		expect((await response.arrayBuffer()).byteLength).toBe(100);
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
	});

	it('returns 416 for a zero-length suffix range', async () => {
		const response = await getAudio('bytes=-0');

		expect(response.status).toBe(416);
		expect(response.headers.get('content-range')).toBe(`bytes */${MEDIA_BYTES}`);
		expect(response.headers.get('content-length')).toBe('0');
	});
});
