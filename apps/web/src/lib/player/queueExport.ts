import { writable } from 'svelte/store';
import { createTrackListExport } from './trackListExport.js';
import { formatQueueM3U, formatQueueText, type QueueState } from './queue.js';

interface QueueExportDeps {
	getQueue: () => QueueState;
	isActive: () => boolean;
}

/** Export intent and temporary URLs belong to one layout, not its queue view. */
export function createQueueExport(deps: QueueExportDeps) {
	const store = writable<{ status: string; fallback: string | null }>({ status: '', fallback: null });
	const actions = createTrackListExport({
		...deps,
		onOutcome: (status, fallback) => store.set({ status, fallback }),
		formatText: formatQueueText, formatM3U: formatQueueM3U,
		noun: 'queue', filename: 'queue.m3u'
	});
	return { subscribe: store.subscribe, copy: actions.copy, download: actions.download, reset: actions.reset };
}
