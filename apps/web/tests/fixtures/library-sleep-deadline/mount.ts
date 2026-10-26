import { mount, tick, unmount } from 'svelte';
import Library from '../../../src/routes/library/+page.svelte';
import { notifyLocalAuthTransition } from '../../../src/lib/auth-transition.js';
import { selectTrack, type QueueState } from '../../../src/lib/player/queue.js';
import '../../../src/app.css';

// The test-only pre-transform exports the EXISTING queue setter, not timer logic.
let component = mount(Library, { target: document.getElementById('app')! }) as unknown as {
	acceptanceReplaceQueue(state: QueueState): void;
};
let state: QueueState = { tracks: [], currentIndex: -1 };
async function replace(next: QueueState) {
	state = next;
	component.acceptanceReplaceQueue(next);
	await tick();
}

declare global {
	interface Window {
		librarySleep: {
			replace(next: QueueState): Promise<void>;
			select(index: number): Promise<void>;
			auth(): Promise<void>;
			leave(): Promise<void>;
		};
	}
}
window.librarySleep = {
	replace,
	select: (index) => replace(selectTrack(state, index)),
	auth: async () => { notifyLocalAuthTransition(); await tick(); },
	leave: async () => { await unmount(component); await tick(); }
};
