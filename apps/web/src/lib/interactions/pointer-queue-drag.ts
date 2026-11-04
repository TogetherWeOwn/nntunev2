import { listen, stopListening } from './listen.js';
import { createTrackGhost } from './track-drag.js';

interface QueueDragOptions {
	ids: () => string[];
	list: () => HTMLElement | null;
	/** Replacing the authorized arrangement cancels a captured touch intent. */
	version: unknown;
	hover: (target: Element, y: number) => void;
	drop: (ids: string[], target: Element, y: number) => void;
	cancel: () => void;
}

/** Touch/pen dragging is grip-only; the rest of the list retains native scrolling. */
export function pointerQueueDrag(node: HTMLElement, initial: QueueDragOptions) {
	let options = initial;
	let pointer: number | null = null;
	let origin = { x: 0, y: 0 };
	let ids: string[] = [];
	let ghost: HTMLElement | null = null;
	let position = { x: 0, y: 0 };
	let frame = 0;
	let lastFrame = 0;
	let pendingListeners: Array<() => void> = [];

	function scroll(time: number) {
		if (pointer === null || !ghost) return;
		const elapsed = Math.min(32, lastFrame ? time - lastFrame : 16);
		lastFrame = time;
		const list = options.list();
		const bounds = list?.getBoundingClientRect();
		if (list && bounds && position.x >= bounds.left && position.x <= bounds.right) {
			// Prefer the nearest scrolling pane, then its ancestors at an edge.
			for (let element: HTMLElement | null = list; element; element = element.parentElement) {
				const page = element === document.scrollingElement;
				if (!page && !/(auto|scroll)/.test(getComputedStyle(element).overflowY)) continue;
				if (element.scrollHeight <= element.clientHeight) continue;
				const rect = page ? { top: 0, bottom: window.innerHeight } : element.getBoundingClientRect();
				const top = Math.max(0, rect.top);
				const bottom = Math.min(window.innerHeight, rect.bottom);
				if (position.y < top || position.y > bottom) continue;
				const edge = Math.min(48, (bottom - top) / 3);
				if (edge <= 0) continue;
				const speed = position.y < top + edge ? -(top + edge - position.y) / edge
					: position.y > bottom - edge ? (position.y - bottom + edge) / edge : 0;
				const before = element.scrollTop;
				element.scrollBy({ top: speed * elapsed, behavior: 'instant' });
				if (element.scrollTop !== before) break;
			}
		}
		const target = targetAt(position.x, position.y);
		if (target) options.hover(target, position.y);
		else options.cancel();
		frame = requestAnimationFrame(scroll);
	}

	function finish() {
		if (pointer === null) return;
		const id = pointer;
		pointer = null;
		cancelAnimationFrame(frame);
		frame = 0;
		lastFrame = 0;
		if (node.hasPointerCapture(id)) node.releasePointerCapture(id);
		ghost?.remove();
		ghost = null;
		delete document.documentElement.dataset.trackDragging;
		options.cancel();
		stopListening(pendingListeners);
		pendingListeners = [];
	}
	function key(event: KeyboardEvent) { if (event.key === 'Escape') { event.preventDefault(); finish(); } }
	function hidden() { if (document.hidden) finish(); }
	function targetAt(x: number, y: number): Element | null {
		const target = document.elementFromPoint(x, y);
		return target && options.list()?.contains(target) ? target : null;
	}
	function down(event: PointerEvent) {
		if (event.pointerType === 'mouse' || !event.isPrimary || event.button !== 0) return;
		finish();
		ids = options.ids();
		if (!ids.length || !options.list()) return;
		event.preventDefault();
		pointer = event.pointerId;
		origin = { x: event.clientX, y: event.clientY };
		node.setPointerCapture(pointer);
		pendingListeners = [
			listen(window, 'blur', finish),
			listen(document, 'keydown', key),
			listen(document, 'visibilitychange', hidden)
		];
	}
	function move(event: PointerEvent) {
		if (pointer !== event.pointerId) return;
		if (!ghost && Math.hypot(event.clientX - origin.x, event.clientY - origin.y) < 6) return;
		event.preventDefault();
		position = { x: event.clientX, y: event.clientY };
		if (!ghost) {
			ghost = createTrackGhost(ids.length);
			document.documentElement.dataset.trackDragging = 'queue';
			frame = requestAnimationFrame(scroll);
		}
		ghost.style.left = `${Math.max(0, Math.min(event.clientX + 12, window.innerWidth - ghost.offsetWidth))}px`;
		ghost.style.top = `${Math.max(0, Math.min(event.clientY + 12, window.innerHeight - ghost.offsetHeight))}px`;
		const target = targetAt(event.clientX, event.clientY);
		if (target) options.hover(target, event.clientY);
		else options.cancel();
	}
	function up(event: PointerEvent) {
		if (pointer !== event.pointerId) return;
		const target = ghost ? targetAt(event.clientX, event.clientY) : null;
		const selected = ids;
		finish();
		if (target) options.drop(selected, target, event.clientY);
	}
	function cancel(event: PointerEvent) { if (pointer === event.pointerId) finish(); }
	const listeners = [
		listen(node, 'pointerdown', down),
		listen(node, 'pointermove', move),
		listen(node, 'pointerup', up),
		listen(node, 'pointercancel', cancel),
		listen(node, 'lostpointercapture', cancel)
	];
	return {
		update(next: QueueDragOptions) {
			if (next.version !== options.version) finish();
			options = next;
		},
		destroy() { finish(); stopListening(listeners); }
	};
}
