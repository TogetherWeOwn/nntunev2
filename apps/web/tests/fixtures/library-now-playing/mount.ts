import { mount, tick } from 'svelte';
import Library from '../../../src/routes/library/+page.svelte';
import { moveTrack, restoreQueue, selectTrack, type QueueState, type Track } from '../../../src/lib/player/queue.js';
import '../../../src/app.css';

/** Test-only bridge; the Vite pre-transform exports the route's existing setter. */
const library = mount(Library, { target: document.getElementById('app')! }) as unknown as {
	acceptanceReplaceQueue(state: QueueState): void;
};
let state: QueueState = { tracks: [], currentIndex: -1 };
async function replace(next: QueueState) {
	state = next;
	library.acceptanceReplaceQueue(next);
	await tick();
}

declare global {
	interface Window {
		libraryNowPlaying: {
			replace(state: QueueState): Promise<void>;
			select(index: number): Promise<void>;
			move(from: number, to: number): Promise<void>;
			restore(raw: string, catalogue: Track[]): Promise<void>;
		};
		libraryNowPlayingAnnouncements: string[];
	}
}

// No playback, readout, announcement or restore logic is invented by the harness.
// These are the real reducers feeding the real page's existing replaceQueue.
window.libraryNowPlaying = {
	replace,
	select: (index) => replace(selectTrack(state, index)),
	move: (from, to) => replace(moveTrack(state, from, to)),
	restore: (raw, catalogue) => replace(restoreQueue(raw, catalogue))
};
