import { mount, tick } from 'svelte';
import QueueParent from './QueueParent.svelte';
import type { QueueState } from '../../../src/lib/player/queue.js';

export type QueueOrientationBridge = {
	replace(value: QueueState): Promise<void>;
	inspect(): { state: QueueState; nextCalls: number };
};

declare global {
	interface Window {
		queueOrientation: QueueOrientationBridge;
		orientationProbe: { media: string[]; storage: string[]; scroll: string[] };
	}
}

const parent = mount(QueueParent, { target: document.getElementById('app')! });
window.queueOrientation = {
	async replace(value) {
		value.tracks.forEach((track) => Object.freeze(track));
		Object.freeze(value.tracks);
		Object.freeze(value);
		parent.replace(value);
		await tick();
	},
	inspect: () => parent.inspect()
};
