import { currentTrack, type QueueState } from './queue.js';
import { buildShareQuery } from './shareTimestamp.js';

interface QueueShareDeps {
	getQueue: () => QueueState;
	getPosition: () => number | undefined;
	getOrigin: () => string;
	getClipboard: () => { writeText: (text: string) => Promise<void> } | undefined;
	shortTitle: (title: string) => string;
	onOutcome: (status: string, fallbackUrl: string | null, queue: QueueState) => void;
}

/** Owns one host's queue-row copy lifecycle; the host owns presentation. */
export function createQueueShare(deps: QueueShareDeps) {
	let generation = 0;
	let alive = true;
	function outcome(status: string, fallbackUrl: string | null): void {
		deps.onOutcome(status, fallbackUrl, deps.getQueue());
	}
	function reset(): void {
		if (!alive) return;
		generation += 1;
		outcome('', null);
	}
	async function copy(id: string): Promise<void> {
		if (!alive) return;
		const action = generation + 1;
		const isCurrent = () => alive && action === generation;
		reset();
		if (!isCurrent()) return;
		const queue = deps.getQueue();
		const queued = queue.tracks.find((track) => track.id === id);
		if (!queued) {
			outcome('Link unavailable — that track is no longer in the queue.', null);
			return;
		}
		const title = deps.shortTitle(queued.title?.trim() ? queued.title : queued.id);
		const unavailable = `Link unavailable — ${title} is no longer in the queue.`;
		const current = currentTrack(queue);
		const position = current?.id === id ? deps.getPosition() : 0;
		const seconds = typeof position === 'number' && Number.isFinite(position) && position >= 0 ? position : 0;
		let url: string;
		try {
			url = `${deps.getOrigin()}/library?${buildShareQuery(id, seconds)}`;
		} catch {
			if (isCurrent()) outcome(unavailable, null);
			return;
		}
		const clipboard = deps.getClipboard();
		if (!isCurrent()) return;
		if (!clipboard) {
			outcome('', url);
			return;
		}
		let written = true;
		try {
			await clipboard.writeText(url);
		} catch {
			written = false;
		}
		if (!isCurrent()) return;
		if (!deps.getQueue().tracks.some((track) => track.id === id)) {
			outcome(unavailable, null);
			return;
		}
		if (written) outcome(`Copied link for ${title}.`, null);
		else outcome('', url);
	}
	return { copy, reset, dispose: () => { alive = false; generation += 1; } };
}
