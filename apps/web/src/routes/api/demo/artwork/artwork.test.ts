import { expect, it } from 'vitest';
import { GET } from './+server.js';

it('serves SVG artwork with public immutable caching', async () => {
	const request = new Request('https://example.test/api/demo/artwork');
	const response = await GET({ request, url: new URL(request.url) } as Parameters<typeof GET>[0]);

	expect(response.status).toBe(200);
	expect(response.headers.get('content-type')).toBe('image/svg+xml');
	const cacheControl = response.headers.get('cache-control');
	expect(cacheControl).toMatch(/^public/);
	expect(cacheControl).toContain('immutable');
});
