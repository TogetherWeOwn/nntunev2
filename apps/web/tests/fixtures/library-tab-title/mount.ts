import { mount, tick, unmount } from 'svelte';
import Host from './Host.svelte';
import type { QueueState } from '../../../src/lib/player/queue.js';
import '../../../src/app.css';

// The one-time same-PR owner-wiring exception mounts the real session and queue.
// All other acceptance protection remains; the input exports no title logic.
const library = mount(Host, { target: document.getElementById('app')! }) as unknown as {
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
