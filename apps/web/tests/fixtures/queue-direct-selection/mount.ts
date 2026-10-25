import { mount, tick } from 'svelte';
import QueueParent from './QueueParent.svelte';
import type { QueueState } from '../../../src/lib/player/queue.js';

declare global {
	interface Window {
		queuePick: {
			replace(value: QueueState, available?: boolean): Promise<void>;
			inspect(): { state: QueueState; calls: number[]; nextCalls: number };
		};
		pickProbe: { media: string[]; sources: string[]; storage: string[] };
	}
}

const parent = mount(QueueParent, { target: document.getElementById('app')! });
window.queuePick = {
	async replace(value, available = true) {
		value.tracks.forEach(Object.freeze);
		Object.freeze(value.tracks);
		Object.freeze(value);
		parent.replace(value, available);
		await tick();
	},
	inspect: () => parent.inspect()
};
