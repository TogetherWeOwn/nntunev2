import { subscribeLocalAuthTransition } from '../auth-transition.js';
import { LIBRARY_AUTH_TRANSITION_KEY } from '../library/cache.js';

let generation = $state(0);
let listening = false;

/** One listener for the browser module lifetime, installed before the first read. */
export function catalogueAuthGeneration(): number {
	if (typeof window !== 'undefined' && !listening) {
		listening = true;
		const reset = () => { generation += 1; };
		subscribeLocalAuthTransition(reset);
		window.addEventListener('storage', (event) => {
			if (event.key === LIBRARY_AUTH_TRANSITION_KEY || event.key === null) reset();
		});
	}
	return generation;
}
