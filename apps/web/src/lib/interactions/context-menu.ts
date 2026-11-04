import { listen, stopListening } from './listen.js';

export type MenuAction = {
	label: string;
	checked?: boolean;
	destructive?: boolean;
	disabled?: boolean;
	hint?: string;
} & ({ run: () => void; children?: never } | { children: MenuAction[]; run?: never });

/** Reserved until routed playlist endpoints and a picker are available. */
export const unavailablePlaylistAction: MenuAction = {
	label: 'Add to playlist', disabled: true, hint: 'Not available yet', run: () => {}
};

/** One-choice groups add no navigation; keep their sole action inline. */
export function inlineSingleActions(actions: readonly MenuAction[]): MenuAction[] {
	return actions.flatMap<MenuAction>((action) => {
		if (!action.children) return [action];
		const children = inlineSingleActions(action.children);
		if (children.length === 1) return [{ ...children[0], disabled: action.disabled || children[0].disabled }];
		return [{ ...action, children }];
	});
}

export interface MenuRequest {
	label: string;
	actions: MenuAction[];
	initialIndex?: number;
	fallbackFocus?: () => void;
	invoker: HTMLElement;
	x: number;
	y: number;
}

export function menuAt(invoker: HTMLElement, label: string, actions: MenuAction[]): MenuRequest {
	const rect = invoker.getBoundingClientRect();
	return { invoker, label, actions, x: rect.left, y: rect.bottom };
}

/** Flip above the origin if needed, then clamp both axes inside the viewport. */
export function menuPosition(x: number, y: number, width: number, height: number, viewportWidth: number, viewportHeight: number) {
	return {
		left: Math.max(0, Math.min(x, viewportWidth - width)),
		top: Math.max(0, Math.min(y + height > viewportHeight ? y - height : y, viewportHeight - height))
	};
}

/** Right click, Menu/Shift+F10 and cancellable 500 ms long-press share one intent. */
export function contextMenu(node: HTMLElement, open: ((invoker: HTMLElement, point?: { x: number; y: number }) => void) | undefined) {
	let callback = open;
	let timer: ReturnType<typeof setTimeout> | undefined;
	let origin: { x: number; y: number } | null = null;
	let pressed = false;
	let pendingListeners: Array<() => void> = [];
	function cancel() {
		clearTimeout(timer);
		timer = undefined;
		origin = null;
		stopListening(pendingListeners);
		pendingListeners = [];
	}
	function abort() { pressed = false; cancel(); }
	function hidden() { if (document.hidden) abort(); }
	function show(event: MouseEvent) {
		if (!callback) return;
		event.preventDefault();
		event.stopPropagation();
		cancel();
		callback(node, { x: event.clientX, y: event.clientY });
	}
	function key(event: KeyboardEvent) {
		if (!callback || (event.key !== 'ContextMenu' && !(event.key === 'F10' && event.shiftKey))) return;
		event.preventDefault();
		event.stopPropagation();
		const invoker = event.target instanceof HTMLElement ? event.target : node;
		callback(invoker);
	}
	function down(event: PointerEvent) {
		cancel();
		// A second contact cancels a pending press but must not release the
		// compatibility-click guard of a menu that already opened.
		if (!event.isPrimary) return;
		pressed = false;
		// Primary row controls opt in; form controls and secondary actions keep
		// their own press, selection and scroll behavior.
		if (!callback || event.pointerType === 'mouse' || event.button !== 0 ||
			(event.target instanceof Element && event.target.closest('button, input, a, select, textarea') &&
			 !event.target.closest('[data-context-menu-longpress]'))) return;
		origin = { x: event.clientX, y: event.clientY };
		// Listen beyond the row only while a press is pending: an interrupted
		// gesture must not open a menu later, after its pointer or page left.
		pendingListeners = [
			listen(window, 'blur', cancel),
			listen(document, 'visibilitychange', hidden),
			listen(document, 'scroll', cancel, true),
			listen(document, 'pointerup', cancel),
			listen(document, 'pointercancel', abort)
		];
		timer = setTimeout(() => {
			pressed = true;
			callback?.(node, origin ?? undefined);
			cancel();
		}, 500);
	}
	function move(event: PointerEvent) {
		if (origin && Math.hypot(event.clientX - origin.x, event.clientY - origin.y) > 10) cancel();
	}
	function mouseDown(event: MouseEvent) {
		// A long press on a button must not refocus it when the browser emits
		// compatibility mouse events on release, dismissing its open menu.
		if (pressed) event.preventDefault();
	}
	function click(event: MouseEvent) {
		if (!pressed) return;
		pressed = false;
		// Keyboard activation has no compatibility mouse click to suppress.
		if (event.detail === 0) return;
		event.preventDefault();
		event.stopPropagation();
	}
	const listeners = [
		listen(node, 'contextmenu', show),
		listen(node, 'keydown', key),
		listen(node, 'pointerdown', down),
		listen(node, 'pointermove', move),
		listen(node, 'pointerup', cancel),
		listen(node, 'pointercancel', abort),
		listen(node, 'dragstart', cancel),
		listen(node, 'mousedown', mouseDown, true),
		listen(node, 'click', click, true)
	];
	return {
		update(next: typeof open) { cancel(); pressed = false; callback = next; },
		destroy() { cancel(); stopListening(listeners); }
	};
}
