import adapter from '@sveltejs/adapter-cloudflare';
import { beforeEach, expect, test, vi } from 'vitest';
import configure from '../vite.config.js';

vi.mock('@sveltejs/adapter-cloudflare', { spy: true });

beforeEach(() => {
	vi.mocked(adapter).mockClear();
});

test('disables Miniflare persistence for browser fixture previews', () => {
	configure({ command: 'serve', mode: 'browser-fixtures', isPreview: true });
	expect(adapter).toHaveBeenCalledExactlyOnceWith({ platformProxy: { persist: false } });
});

test.each([
	['an ordinary preview', { command: 'serve', mode: 'production', isPreview: true }],
	['fixture development', { command: 'serve', mode: 'browser-fixtures', isPreview: false }],
	['a fixture build', { command: 'build', mode: 'browser-fixtures', isPreview: false }]
] as const)('uses the Wrangler default for %s', (_case, config) => {
	configure(config);
	expect(adapter).toHaveBeenCalledTimes(1);
	expect(vi.mocked(adapter).mock.lastCall?.[0]).toBeUndefined();
});
