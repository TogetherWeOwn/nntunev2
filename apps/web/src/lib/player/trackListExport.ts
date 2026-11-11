import { createM3UDownload, m3uDownloadFailure } from './m3uDownload.js';
import { clipboardWriter } from '../clipboard.js';
import type { QueueState } from './queue.js';

interface TrackListExportDeps {
	getQueue: () => QueueState;
	/** Snapshot identity by default; selection/favorites use their metadata key. */
	getKey?: () => unknown;
	isActive: () => boolean;
	onOutcome: (status: string, fallback: string | null) => void;
	formatText: (queue: QueueState) => string;
	formatM3U: (queue: QueueState) => string;
	noun: string;
	filename: string;
	/** Selection/favorites must reject unsafe ids on both output paths. */
	unsafeMessage?: string;
	savedDownloads?: boolean;
}

/** Shared export mechanics; each owner still supplies its own lifetime and snapshot. */
export function createTrackListExport(deps: TrackListExportDeps) {
	let generation = 0;
	const file = createM3UDownload();
	const key = deps.getKey ?? deps.getQueue;

	function reset(release = false): number {
		const operation = ++generation;
		deps.onOutcome('', null);
		if (release) file.release();
		return operation;
	}

	function begin() {
		const operation = reset();
		const queue = deps.getQueue();
		const identity = key();
		return {
			queue,
			current: () => deps.isActive() && operation === generation && identity === key()
		};
	}

	async function copy(): Promise<void> {
		const { queue, current } = begin();
		if (!current() || queue.tracks.length === 0) return;
		let text: string;
		try {
			if (deps.unsafeMessage) deps.formatM3U(queue);
			text = deps.formatText(queue);
		} catch {
			if (current() && deps.unsafeMessage) deps.onOutcome(deps.unsafeMessage, null);
			return;
		}
		try {
			// Accessors and writes may both refuse. Never claim a copy on failure.
			const clipboard = clipboardWriter();
			if (!clipboard) throw new Error('No clipboard');
			if (!current()) return;
			await clipboard.writeText(text);
		} catch {
			if (current()) deps.onOutcome(`Clipboard unavailable. Copy the ${deps.noun} manually below.`, text);
			return;
		}
		if (current()) deps.onOutcome(`Copied ${deps.noun} as text.`, null);
	}

	function download(): void {
		const { queue, current } = begin();
		if (!current() || queue.tracks.length === 0) return;
		let m3u: string | undefined;
		if (deps.unsafeMessage) {
			try {
				m3u = deps.formatM3U(queue);
			} catch {
				if (current()) deps.onOutcome(deps.unsafeMessage, null);
				return;
			}
		}
		if (!current()) return;
		try {
			const status = file.download(() => m3u ?? deps.formatM3U(queue), deps.filename, deps.savedDownloads);
			if (current()) deps.onOutcome(status, null);
		} catch {
			file.release();
			if (current()) deps.onOutcome(m3uDownloadFailure(deps.noun), null);
		}
	}

	return { copy, download, reset, release: file.release };
}
