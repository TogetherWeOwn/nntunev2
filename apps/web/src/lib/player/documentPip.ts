/**
 * Document Picture-in-Picture window lifecycle, kept free of Svelte and
 * any playback state so the open/close fences are unit-testable. The PiP
 * window is a remote control only: no media element, queue state, gain
 * graph, Media Session or storage ever moves into it, and closing it has
 * no playback side effect — this module does not know playback exists.
 * Consumers mount a view from `onOpen` and tear it down from `onClose`.
 */

/** The slice of a Document PiP window the controller drives. */
export interface DocumentPipWindowLike {
	close(): void;
	addEventListener(type: string, listener: () => void): void;
	removeEventListener?(type: string, listener: () => void): void;
}

export interface DocumentPipRequestOptions {
	width: number;
	height: number;
}

/** Structural subset of `window.documentPictureInPicture`. */
export interface DocumentPipLike {
	requestWindow(options: DocumentPipRequestOptions): Promise<DocumentPipWindowLike>;
}

/**
 * Structural subset of the opener `window` used for feature detection.
 * `self`/`top` are compared only when both are present, so an injected
 * host without frame references is taken to be the top-level window its
 * caller says it is; a real `window` always carries both.
 */
export interface DocumentPipHostLike {
	documentPictureInPicture?: DocumentPipLike;
	isSecureContext?: boolean;
	self?: unknown;
	top?: unknown;
}

export interface DocumentPipControllerDeps {
	/** Defaults to the global `window` when present. Null = unsupported. */
	host?: DocumentPipHostLike | null;
	/** Initial size request; the browser may clamp it. */
	width?: number;
	height?: number;
	/** Called once per successful open, after the reference is held. */
	onOpen?: (pipWindow: DocumentPipWindowLike) => void;
	/** Teardown: called exactly once per successful open, on any close path. */
	onClose?: () => void;
	/** A failed open, for the page's polite feedback. Playback is untouched. */
	onError?: (error: unknown) => void;
}

export interface DocumentPipController {
	isSupported(): boolean;
	isOpen(): boolean;
	currentWindow(): DocumentPipWindowLike | null;
	/** Resolves true when a window is open (already open counts). */
	open(): Promise<boolean>;
	/** Opens when closed, closes when open; a toggle during an in-flight
	 * request cancels it and resolves false once the request settles. */
	toggle(): Promise<boolean>;
	close(): void;
	/** Close for good: every later call and retained callback is inert. */
	dispose(): void;
}

export const DOCUMENT_PIP_DEFAULT_WIDTH = 400;
export const DOCUMENT_PIP_DEFAULT_HEIGHT = 240;

function defaultHost(): DocumentPipHostLike | null {
	const host = (globalThis as { window?: DocumentPipHostLike }).window;
	return host ?? null;
}

/**
 * Supported only with the API present, in a secure context, from the
 * top-level window. No user-agent sniffing.
 */
export function isDocumentPipSupported(host?: DocumentPipHostLike | null): boolean {
	const resolved = host === undefined ? defaultHost() : host;
	if (!resolved) return false;
	if (!('documentPictureInPicture' in resolved) || !resolved.documentPictureInPicture) {
		return false;
	}
	if (resolved.isSecureContext !== true) return false;
	if (resolved.self !== undefined && resolved.top !== undefined && resolved.self !== resolved.top) {
		return false;
	}
	return true;
}

export function createDocumentPipController(
	deps: DocumentPipControllerDeps = {}
): DocumentPipController {
	const {
		width = DOCUMENT_PIP_DEFAULT_WIDTH,
		height = DOCUMENT_PIP_DEFAULT_HEIGHT,
		onOpen,
		onClose,
		onError
	} = deps;
	// One live window per tab at most: `pipWindow` is the held reference,
	// `pending` coalesces concurrent opens, and every close-path bumps
	// `generation` so an async continuation that wakes under a newer
	// intent is inert (a window that arrives after close()/dispose() is
	// closed on arrival instead of being adopted).
	let pipWindow: DocumentPipWindowLike | null = null;
	let pending: Promise<boolean> | null = null;
	let generation = 0;
	let disposed = false;
	let detachPagehide: () => void = () => {};

	function host(): DocumentPipHostLike | null {
		return deps.host === undefined ? defaultHost() : deps.host;
	}

	function reportError(error: unknown): void {
		try {
			onError?.(error);
		} catch {
			// Consumer feedback must never break the lifecycle fences.
		}
	}

	function fireClose(): void {
		try {
			onClose?.();
		} catch {
			// Teardown errors are the consumer's; the window still closes.
		}
	}

	function closeWindowQuietly(win: DocumentPipWindowLike): void {
		try {
			win.close();
		} catch {
			// Already gone; the reference is dropped either way.
		}
	}

	/** Drop the held reference, run teardown once, optionally close it. */
	function releaseCurrent(closeIt: boolean): void {
		const win = pipWindow;
		if (!win) return;
		pipWindow = null;
		generation += 1;
		detachPagehide();
		detachPagehide = () => {};
		fireClose();
		if (closeIt) closeWindowQuietly(win);
	}

	function handlePagehide(win: DocumentPipWindowLike): void {
		if (pipWindow !== win) return;
		// The browser is already closing this window; closing a remote
		// control only drops the reference and tears the view down.
		releaseCurrent(false);
	}

	function open(): Promise<boolean> {
		if (disposed) return Promise.resolve(false);
		if (pipWindow) return Promise.resolve(true);
		if (pending) return pending;
		const resolved = host();
		if (!isDocumentPipSupported(resolved)) return Promise.resolve(false);
		const api = resolved?.documentPictureInPicture;
		if (!api) return Promise.resolve(false);

		const gen = (generation += 1);
		const promise = (async (): Promise<boolean> => {
			let win: DocumentPipWindowLike;
			try {
				win = await api.requestWindow({ width, height });
			} catch (error) {
				if (gen === generation && !disposed) reportError(error);
				return false;
			}
			if (gen !== generation || disposed) {
				closeWindowQuietly(win);
				return false;
			}
			pipWindow = win;
			const listener = () => handlePagehide(win);
			try {
				win.addEventListener('pagehide', listener);
				detachPagehide = () => {
					try {
						win.removeEventListener?.('pagehide', listener);
					} catch {
						// The identity fence in handlePagehide covers a stuck listener.
					}
				};
			} catch {
				detachPagehide = () => {};
			}
			try {
				onOpen?.(win);
			} catch (error) {
				// A consumer that cannot mount its view fails the open:
				// drop the reference, close the blank window, report it.
				// No onClose — it pairs only with a successful onOpen.
				pipWindow = null;
				generation += 1;
				detachPagehide();
				detachPagehide = () => {};
				closeWindowQuietly(win);
				reportError(error);
				return false;
			}
			return true;
		})();
		pending = promise;
		void promise.finally(() => {
			if (pending === promise) pending = null;
		});
		return promise;
	}

	function close(): void {
		if (disposed) return;
		// Cancels an in-flight request too; `pending` stays until it settles
		// so no second request can start while the first is outstanding.
		generation += 1;
		releaseCurrent(true);
	}

	function toggle(): Promise<boolean> {
		if (disposed) return Promise.resolve(false);
		if (pipWindow) {
			close();
			return Promise.resolve(false);
		}
		if (pending) {
			generation += 1;
			return pending;
		}
		return open();
	}

	function dispose(): void {
		if (disposed) return;
		disposed = true;
		generation += 1;
		releaseCurrent(true);
	}

	return {
		isSupported: () => isDocumentPipSupported(host()),
		isOpen: () => pipWindow !== null,
		currentWindow: () => pipWindow,
		open,
		toggle,
		close,
		dispose
	};
}
