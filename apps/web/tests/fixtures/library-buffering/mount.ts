import { mount, tick, unmount } from 'svelte';
import Library from '../../../src/routes/library/+page.svelte';
import type { QueueState } from '../../../src/lib/player/queue.js';
import '../../../src/app.css';

// The test transform exposes only the route's existing queue input. Production
// markup, native listeners, lifecycle policy and transport are never replaced.
const library = mount(Library, { target: document.getElementById('app')! }) as unknown as {
	acceptanceReplaceQueue(state: QueueState): void;
};

declare global {
	interface Window {
		libraryBuffering: {
			replace(state: QueueState): Promise<void>;
			destroy(): Promise<void>;
		};
	}
}

window.libraryBuffering = {
	async replace(next) { library.acceptanceReplaceQueue(next); await tick(); },
	async destroy() { await unmount(library); await tick(); }
};
