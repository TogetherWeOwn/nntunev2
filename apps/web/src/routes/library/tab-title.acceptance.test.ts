import { render } from 'svelte/server';
import { afterEach, describe, expect, it, vi } from 'vitest';
import Library from './+page.svelte';

/** LIBTITLE protected SSR controls; paired with tests/library-tab-selection-title.spec.ts.
 * Breaks: accessing browser globals/storage or starting title timers/fetch during
 * SSR, leaking a previous request's selected metadata into the server title.
 * No missing feature imports or source-shape checks: render the real route.
 */
afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); });

describe('library selected-track tab title SSR boundary', () => {
	it('LIBTITLE-18 renders exact baseline without browser, storage, fetch or title timers', () => {
		vi.stubGlobal('window', undefined);
		vi.stubGlobal('document', undefined);
		const storage = new Proxy({}, { get() { throw new Error('SSR touched browser storage'); } });
		vi.stubGlobal('localStorage', storage);
		vi.stubGlobal('sessionStorage', storage);
		const fetch = vi.fn(() => { throw new Error('SSR started a library/title request'); });
		vi.stubGlobal('fetch', fetch);
		const timeout = vi.spyOn(globalThis, 'setTimeout');
		const interval = vi.spyOn(globalThis, 'setInterval');
		const result = render(Library);
		expect(result.head.match(/<title[^>]*>([^<]*)<\/title>/)?.[1]).toBe('Library — NNTune');
		expect(result.head).not.toContain('Selected:');
		expect(fetch).not.toHaveBeenCalled();
		expect(timeout).not.toHaveBeenCalled();
		expect(interval).not.toHaveBeenCalled();
	});

	it('LIBTITLE-19 independent server renders do not retain a previous selection or title owner', () => {
		const first = render(Library);
		const second = render(Library);
		for (const result of [first, second]) {
			expect(result.head.match(/<title[^>]*>([^<]*)<\/title>/)?.[1]).toBe('Library — NNTune');
			expect(result.body).not.toContain('aria-current="true"');
		}
	});
});
