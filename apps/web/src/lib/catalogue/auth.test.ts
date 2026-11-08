import { afterEach, expect, it, vi } from 'vitest';
import { notifyLocalAuthTransition } from '../auth-transition.js';
import { LIBRARY_AUTH_TRANSITION_KEY } from '../library/cache.js';
import { catalogueAuthGeneration } from './auth.svelte.js';

afterEach(() => vi.unstubAllGlobals());

it('installs once before reads and advances only for delivered auth transitions', () => {
	const window = new EventTarget();
	vi.stubGlobal('window', window);
	const listen = vi.spyOn(window, 'addEventListener');
	const initial = catalogueAuthGeneration();
	expect(catalogueAuthGeneration()).toBe(initial);
	expect(listen).toHaveBeenCalledTimes(1);
	notifyLocalAuthTransition();
	expect(catalogueAuthGeneration()).toBe(initial + 1);

	const storage = (key: string | null) => {
		window.dispatchEvent(Object.assign(new Event('storage'), { key }));
	};
	storage('unrelated-preference');
	expect(catalogueAuthGeneration()).toBe(initial + 1);
	storage(LIBRARY_AUTH_TRANSITION_KEY);
	expect(catalogueAuthGeneration()).toBe(initial + 2);
	storage(null);
	expect(catalogueAuthGeneration()).toBe(initial + 3);
	expect(listen).toHaveBeenCalledTimes(1);
});
