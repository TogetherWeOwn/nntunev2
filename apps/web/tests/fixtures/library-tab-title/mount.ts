import { mount, tick, unmount } from 'svelte';
import LibraryPlayerHarness from '../LibraryPlayerHarness.svelte';
import type { QueueState } from '../../../src/lib/player/queue.js';
import '../../../src/app.css';

// Test-only input seam, identical to the native-now-playing fixture pattern.
// The pre-transform exports the existing replaceQueue, never title logic.
const library = mount(LibraryPlayerHarness, { target: document.getElementById('app')! });

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
