import { mount, tick } from 'svelte';
import QueueParent from './QueueParent.svelte';

export type QueueConfirmBridge = {
	supply(mode: 'original' | 'none' | 'replacement'): Promise<void>;
	replaceClear(): Promise<void>;
	inspect(): { calls: string[]; tracks: string[]; currentIndex: number };
};

declare global {
	interface Window { queueConfirmHost: QueueConfirmBridge }
}

const parent = mount(QueueParent, { target: document.getElementById('app')! });
window.queueConfirmHost = {
	async supply(mode) {
		parent.supply(mode);
		await tick();
	},
	async replaceClear() {
		parent.replaceClear();
		await tick();
	},
	inspect: () => parent.inspect()
};
