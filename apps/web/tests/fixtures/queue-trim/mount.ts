import { mount, tick } from 'svelte';
import QueueParent from './QueueParent.svelte';
import type { QueueState } from '../../../src/lib/player/queue.js';

export type QueueTrimBridge = {
	replace(value: QueueState, callbacks?: boolean): Promise<void>;
	inspect(): { state: QueueState; calls: string[] };
};

declare global {
	interface Window {
		queueTrim: QueueTrimBridge;
		trimProbe: { media: string[]; positions: number[]; sources: string[] };
	}
}

const parent = mount(QueueParent, { target: document.getElementById('app')! });
window.queueTrim = {
	async replace(value, callbacks = true) {
		value.tracks.forEach(Object.freeze);
		Object.freeze(value.tracks);
		Object.freeze(value);
		parent.replace(value, callbacks);
		await tick();
	},
	inspect: () => parent.inspect()
};
