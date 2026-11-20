import { mount, tick, unmount } from 'svelte';
import Library from '../../../src/routes/library/+page.svelte';
import type { QueueState } from '../../../src/lib/player/queue.js';
import '../../../src/app.css';

// Test-only input seam, identical to the native-now-playing fixture pattern.
// The pre-transform exports the existing replaceQueue, never title logic.
const library = mount(Library, { target: document.getElementById('app')! }) as unknown as {
	acceptanceReplaceQueue(state: QueueState): void;
};

declare global {
	interface Window {
		libraryTabTitle: {
			replace(states: QueueState[]): Promise<void>;
			destroy(): Promise<void>;
		};
		tabTitleProbe: {
			media: string[];
			storage: string[];
			timers: string[];
			scroll: string[];
		};
	}
}

window.libraryTabTitle = {
	async replace(states) {
		for (const state of states) library.acceptanceReplaceQueue(state);
		await tick();
	},
	async destroy() {
		await unmount(library);
		await tick();
	}
};
