import { listen, stopListening } from './listen.js';

const TRACK_TYPE = 'application/x-nntune-track-ids';

export function createTrackGhost(count: number): HTMLElement {
	const ghost = document.createElement('div');
	ghost.textContent = `${count} ${count === 1 ? 'track' : 'tracks'}`;
	ghost.style.cssText = 'position:fixed;top:-1000px;z-index:1000;pointer-events:none;padding:8px;background:var(--nt-bg-raised);color:var(--nt-fg);border:1px solid var(--nt-accent)';
	document.body.append(ghost);
	return ghost;
}

interface TrackDrag {
	ids: string[];
	/** Element identity fences a reorder to the queue that started it. */
	queue: HTMLElement | null;
	token: string;
}

let session: TrackDrag | null = null;
let cleanup: (() => void) | null = null;

export function endTrackDrag(): void {
	session = null;
	cleanup?.();
	cleanup = null;
}

/** Carry only an opaque token outside the page; never trust external track payloads. */
export function startTrackDrag(event: DragEvent, ids: readonly string[], queue: HTMLElement | null = null): void {
	endTrackDrag();
	if (!event.dataTransfer || ids.length === 0) { event.preventDefault(); return; }
	const token = crypto.randomUUID();
	session = { ids: [...new Set(ids)], queue, token };
	event.dataTransfer.setData(TRACK_TYPE, token);
	event.dataTransfer.effectAllowed = 'copyMove';
	document.documentElement.dataset.trackDragging = queue ? 'queue' : 'library';
	const ghost = createTrackGhost(session.ids.length);
	event.dataTransfer.setDragImage(ghost, 0, 0);
	const escape = (key: KeyboardEvent) => {
		if (key.key === 'Escape') endTrackDrag();
	};
	const finish = () => endTrackDrag();
	const hidden = () => { if (document.hidden) finish(); };
	const listeners = [
		listen(document, 'visibilitychange', hidden),
		listen(document, 'keydown', escape, true),
		listen(document, 'dragend', finish),
		listen(document, 'drop', finish),
		listen(window, 'blur', finish)
	];
	cleanup = () => {
		ghost.remove();
		delete document.documentElement.dataset.trackDragging;
		stopListening(listeners);
	};
}

export function activeTrackDrag(event: DragEvent): TrackDrag | null {
	return session && event.dataTransfer?.types.includes(TRACK_TYPE) ? session : null;
}

export function droppedTrackDrag(event: DragEvent): TrackDrag | null {
	const active = activeTrackDrag(event);
	return active && event.dataTransfer?.getData(TRACK_TYPE) === active.token ? active : null;
}
