import { afterEach, beforeEach, vi } from 'vitest';
import * as authFence from '#lib/auth-fence.js';

/** The billing page runs only in a browser. Unit loads supply its captured owner. */
export function useSessionFixture() {
	let restore: (() => void) | undefined;
	beforeEach(() => {
		const { signal } = new AbortController();
		const capture = vi.spyOn(authFence, 'captureAuthFence').mockResolvedValue({
			generation: 'billing-test', signal, isCurrent: () => true,
			run: async (work) => work()
		});
		restore = () => capture.mockRestore();
	});
	afterEach(() => restore?.());
}
