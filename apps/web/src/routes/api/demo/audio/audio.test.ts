import { describe, expect, it } from 'vitest';

import { GET } from './+server.js';

async function getAudio(range?: string): Promise<Response> {
	const request = new Request('https://example.test/api/demo/audio', {
		headers: range ? { range } : undefined
	});
	return GET({ request, url: new URL(request.url) } as Parameters<typeof GET>[0]);
}

describe('demo audio route', () => {
	it('returns a partial response for a satisfiable range', async () => {
		const response = await getAudio('bytes=0-99');

		expect(response.status).toBe(206);
		expect(response.headers.get('content-range')).toBe('bytes 0-99/262144');
		expect((await response.arrayBuffer()).byteLength).toBe(100);
	});

	it('returns 416 with the complete length for an unsatisfiable range', async () => {
		const response = await getAudio('bytes=262144-');

		expect(response.status).toBe(416);
		expect(response.headers.get('content-range')).toBe('bytes */262144');
	});
});
