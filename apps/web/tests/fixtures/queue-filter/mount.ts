import { mount, tick } from 'svelte';
import QueueParent from './QueueParent.svelte';
import type { QueueState } from '../../../src/lib/player/queue.js';

export type QueueFilterCall = { action: string; index?: number; to?: number; id?: string };
export type QueueFilterSnapshot = { state: QueueState; calls: QueueFilterCall[] };

declare global {
	interface Window {
		queueFilter: {
			replace(state: QueueState): Promise<void>;
			inspect(): QueueFilterSnapshot;
		};
		queueFilterProbe: { media: string[]; storage: string[]; scroll: number };
	}
}

const parent = mount(QueueParent, { target: document.getElementById('app')! });
window.queueFilter = {
	async replace(value) {
		value.tracks.forEach(Object.freeze);
		Object.freeze(value.tracks);
		Object.freeze(value);
		parent.replace(value);
		await tick();
	},
	inspect: () => parent.inspect()
};
