/**
 * App-shell toasts (roadmap W10).
 *
 * Device-local, ephemeral status notices with an optional single action
 * (e.g. Undo after removing a favorite). Plain TypeScript following the
 * `onPaywall` listener idiom: module-level state, subscribers get a fresh
 * snapshot on every change, and the root layout bridges the snapshot into
 * its own `$state` for rendering. No storage, no network, no DOM access —
 * safe to import from any route, including during SSR (which simply sees
 * the empty list; pushes only ever happen in client event handlers).
 */

export interface ToastAction {
	label: string;
	run: () => void;
}

export interface Toast {
	id: number;
	message: string;
	action?: ToastAction;
}

/** How long a toast stays up before it dismisses itself. */
export const TOAST_DURATION_MS = 8000;

type ToastListener = (toasts: readonly Toast[]) => void;

let nextId = 1;
let active: Toast[] = [];
const listeners = new Set<ToastListener>();
const timers = new Map<number, ReturnType<typeof setTimeout>>();

function emit(): void {
	const snapshot = [...active];
	for (const listener of listeners) {
		try {
			listener(snapshot);
		} catch {
			// Display is best-effort; a throwing listener must not break the
			// store or starve the remaining subscribers.
		}
	}
}

/**
 * Subscribe to the toast list (root layout). The listener fires immediately
 * with the current snapshot. Returns the unsubscribe.
 */
export function onToasts(listener: ToastListener): () => void {
	listeners.add(listener);
	try {
		listener([...active]);
	} catch {
		// A throwing initial listener still subscribes; the error stays local.
	}
	return () => {
		listeners.delete(listener);
	};
}

/** Read the current toasts without subscribing (tests, non-rendered use). */
export function readToasts(): Toast[] {
	return [...active];
}

function clearTimer(id: number): void {
	const timer = timers.get(id);
	if (timer !== undefined) {
		clearTimeout(timer);
		timers.delete(id);
	}
}

function scheduleDismiss(id: number, durationMs: number): void {
	clearTimer(id);
	timers.set(
		id,
		setTimeout(() => {
			timers.delete(id);
			dismissToast(id);
		}, durationMs)
	);
}

/**
 * Show a toast. Returns a dismiss function for the Undo-style early
 * removal. A non-positive `durationMs` keeps the toast until dismissed
 * (used by tests, never by production callers).
 */
export function pushToast(
	message: string,
	options: { action?: ToastAction; durationMs?: number } = {}
): () => void {
	const durationMs = options.durationMs ?? TOAST_DURATION_MS;
	const toast: Toast = { id: nextId++, message, ...(options.action ? { action: options.action } : {}) };
	active = [...active, toast];
	if (durationMs > 0) scheduleDismiss(toast.id, durationMs);
	emit();
	return () => dismissToast(toast.id);
}

/** Remove one toast; unknown ids are a no-op. */
export function dismissToast(id: number): void {
	clearTimer(id);
	if (!active.some((toast) => toast.id === id)) return;
	active = active.filter((toast) => toast.id !== id);
	emit();
}

/** Remove every toast (tests, auth boundary). */
export function clearToasts(): void {
	for (const id of timers.keys()) clearTimer(id);
	if (active.length === 0) return;
	active = [];
	emit();
}
