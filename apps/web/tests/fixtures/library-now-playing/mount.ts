import { mount, tick } from 'svelte';
import Fixture from './Fixture.svelte';
import { moveTrack, restoreQueue, selectTrack, type QueueState, type Track } from '../../../src/lib/player/queue.js';
import '../../../src/app.css';

/**
 * Engineering Lead authorized a one-time same-PR migration from library-local
 * native controls to the approved persistent custom transport. The bounded
 * executor exception covers this input bridge only; other Builder edits remain
 * prohibited. The Vite transform exports only the route's existing queue setter.
 */
const library = mount(Fixture, { target: document.getElementById('app')! }) as unknown as {
	acceptanceReplaceQueue(state: QueueState): boolean;
};
let state: QueueState = { tracks: [], currentIndex: -1 };
async function replace(next: QueueState) {
	state = next;
	if (!library.acceptanceReplaceQueue(next)) throw new Error('library child not mounted');
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
