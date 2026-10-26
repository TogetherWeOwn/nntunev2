import { describe, expect, it, vi } from 'vitest';
import {
	createDocumentPipController,
	isDocumentPipSupported,
	type DocumentPipHostLike,
	type DocumentPipLike,
	type DocumentPipWindowLike
} from './documentPip.js';

class FakePipWindow implements DocumentPipWindowLike {
	closeCount = 0;
	readonly listeners = new Map<string, Set<() => void>>();

	close(): void {
		this.closeCount += 1;
		// A real window fires pagehide when it closes; the controller must
		// still run its teardown exactly once across both paths.
		this.firePagehide();
	}

	addEventListener(type: string, listener: () => void): void {
		if (!this.listeners.has(type)) this.listeners.set(type, new Set());
		this.listeners.get(type)?.add(listener);
	}

	removeEventListener(type: string, listener: () => void): void {
		this.listeners.get(type)?.delete(listener);
	}

	firePagehide(): void {
		for (const listener of [...(this.listeners.get('pagehide') ?? [])]) listener();
	}
}

function fakeApi(impl?: () => Promise<DocumentPipWindowLike>) {
	const pipWindow = new FakePipWindow();
	const api: DocumentPipLike = {
		requestWindow: vi.fn(impl ?? (async () => pipWindow))
	};
	return { api, pipWindow, request: api.requestWindow as ReturnType<typeof vi.fn> };
}

function fakeHost(api?: DocumentPipLike, overrides: Partial<DocumentPipHostLike> = {}) {
	const frame = {};
	const host: DocumentPipHostLike = {
		isSecureContext: true,
		self: frame,
		top: frame,
		...overrides
	};
	if (api) host.documentPictureInPicture = api;
	return host;
}

function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (error: unknown) => void;
	const promise = new Promise<T>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	return { promise, resolve, reject };
}

describe('isDocumentPipSupported', () => {
	it('PIP-01: reports unsupported when the API is absent from the window', () => {
		expect(isDocumentPipSupported(fakeHost())).toBe(false);
		expect(isDocumentPipSupported(null)).toBe(false);
	});

	it('PIP-01: reports unsupported outside a secure context, even with the API', () => {
		const { api, request } = fakeApi();
		const host = fakeHost(api, { isSecureContext: false });
		expect(isDocumentPipSupported(host)).toBe(false);
		expect(request).not.toHaveBeenCalled();
	});

	it('PIP-01: reports unsupported from a framed (non-top-level) window', () => {
		const { api } = fakeApi();
		const host = fakeHost(api, { self: {}, top: {} });
		expect(isDocumentPipSupported(host)).toBe(false);
	});

	it('reports supported when API, secure context and top-level window align', () => {
		const { api } = fakeApi();
		expect(isDocumentPipSupported(fakeHost(api))).toBe(true);
	});
});

describe('createDocumentPipController', () => {
	it('PIP-01: open() is a silent no-op when unsupported', async () => {
		const onError = vi.fn();
		const controller = createDocumentPipController({ host: fakeHost(), onError });
		expect(controller.isSupported()).toBe(false);
		await expect(controller.open()).resolves.toBe(false);
		expect(controller.isOpen()).toBe(false);
		expect(onError).not.toHaveBeenCalled();
	});

	it('PIP-02: open() calls requestWindow once with both width and height', async () => {
		const { api, pipWindow, request } = fakeApi();
		const onOpen = vi.fn();
		const controller = createDocumentPipController({ host: fakeHost(api), onOpen });

		await expect(controller.open()).resolves.toBe(true);
		expect(request).toHaveBeenCalledOnce();
		expect(request).toHaveBeenCalledWith({
			width: expect.any(Number),
			height: expect.any(Number)
		});
		expect(controller.isOpen()).toBe(true);
		expect(controller.currentWindow()).toBe(pipWindow);
		expect(onOpen).toHaveBeenCalledOnce();
		expect(onOpen).toHaveBeenCalledWith(pipWindow);
	});

	it('PIP-02: honours an injected initial size', async () => {
		const { api, request } = fakeApi();
		const controller = createDocumentPipController({
			host: fakeHost(api),
			width: 320,
			height: 180
		});
		await controller.open();
		expect(request).toHaveBeenCalledWith({ width: 320, height: 180 });
	});

	it('PIP-03: a rejected requestWindow holds no reference and surfaces the error', async () => {
		const rejection = new Error('no transient activation');
		rejection.name = 'NotAllowedError';
		const { api } = fakeApi(async () => {
			throw rejection;
		});
		const onOpen = vi.fn();
		const onClose = vi.fn();
		const onError = vi.fn();
		const controller = createDocumentPipController({
			host: fakeHost(api),
			onOpen,
			onClose,
			onError
		});

		await expect(controller.open()).resolves.toBe(false);
		expect(controller.isOpen()).toBe(false);
		expect(controller.currentWindow()).toBeNull();
		expect(onError).toHaveBeenCalledOnce();
		expect(onError).toHaveBeenCalledWith(rejection);
		expect(onOpen).not.toHaveBeenCalled();
		expect(onClose).not.toHaveBeenCalled();
	});

	it('PIP-04: activating again while open closes (toggle) and never re-requests', async () => {
		const { api, pipWindow, request } = fakeApi();
		const onClose = vi.fn();
		const controller = createDocumentPipController({ host: fakeHost(api), onClose });

		await expect(controller.toggle()).resolves.toBe(true);
		await expect(controller.toggle()).resolves.toBe(false);
		expect(request).toHaveBeenCalledOnce();
		expect(pipWindow.closeCount).toBe(1);
		expect(onClose).toHaveBeenCalledOnce();
		expect(controller.isOpen()).toBe(false);
		expect(controller.currentWindow()).toBeNull();
	});

	it('PIP-04: open() while already open is a no-op that keeps the live window', async () => {
		const { api, pipWindow, request } = fakeApi();
		const controller = createDocumentPipController({ host: fakeHost(api) });

		await controller.open();
		await expect(controller.open()).resolves.toBe(true);
		expect(request).toHaveBeenCalledOnce();
		expect(pipWindow.closeCount).toBe(0);
		expect(controller.currentWindow()).toBe(pipWindow);
	});

	it('coalesces concurrent open() calls into a single requestWindow', async () => {
		const pending = deferred<DocumentPipWindowLike>();
		const pipWindow = new FakePipWindow();
		const { api, request } = fakeApi(() => pending.promise);
		const controller = createDocumentPipController({ host: fakeHost(api) });

		const first = controller.open();
		const second = controller.open();
		pending.resolve(pipWindow);
		await expect(first).resolves.toBe(true);
		await expect(second).resolves.toBe(true);
		expect(request).toHaveBeenCalledOnce();
	});

	it('PIP-05: pagehide drops the reference and fires teardown exactly once', async () => {
		const { api, pipWindow, request } = fakeApi();
		const onClose = vi.fn();
		const controller = createDocumentPipController({ host: fakeHost(api), onClose });
		await controller.open();

		// Browser-chrome close: pagehide without our close() call.
		pipWindow.firePagehide();
		expect(controller.isOpen()).toBe(false);
		expect(controller.currentWindow()).toBeNull();
		expect(onClose).toHaveBeenCalledOnce();
		// The browser is already closing the window; we must not close it again.
		expect(pipWindow.closeCount).toBe(0);

		pipWindow.firePagehide();
		expect(onClose).toHaveBeenCalledOnce();

		// After pagehide the controller can open a fresh window.
		await expect(controller.open()).resolves.toBe(true);
		expect(request).toHaveBeenCalledTimes(2);
	});

	it('PIP-06: dispose while open closes the window exactly once, then stays inert', async () => {
		const { api, pipWindow, request } = fakeApi();
		const onClose = vi.fn();
		const controller = createDocumentPipController({ host: fakeHost(api), onClose });
		await controller.open();

		controller.dispose();
		expect(pipWindow.closeCount).toBe(1);
		expect(onClose).toHaveBeenCalledOnce();
		expect(controller.isOpen()).toBe(false);

		// Everything after dispose is inert: no new request, no second close.
		pipWindow.firePagehide();
		controller.close();
		controller.dispose();
		await expect(controller.open()).resolves.toBe(false);
		await expect(controller.toggle()).resolves.toBe(false);
		expect(request).toHaveBeenCalledOnce();
		expect(pipWindow.closeCount).toBe(1);
		expect(onClose).toHaveBeenCalledOnce();
	});

	it('PIP-06: dispose while a request is pending closes the late window on arrival', async () => {
		const pending = deferred<DocumentPipWindowLike>();
		const pipWindow = new FakePipWindow();
		const { api } = fakeApi(() => pending.promise);
		const onOpen = vi.fn();
		const onClose = vi.fn();
		const controller = createDocumentPipController({
			host: fakeHost(api),
			onOpen,
			onClose
		});

		const opening = controller.open();
		controller.dispose();
		pending.resolve(pipWindow);
		await expect(opening).resolves.toBe(false);
		expect(pipWindow.closeCount).toBe(1);
		expect(onOpen).not.toHaveBeenCalled();
		expect(onClose).not.toHaveBeenCalled();
		expect(controller.isOpen()).toBe(false);
	});

	it('close() while a request is pending closes the late window on arrival', async () => {
		const pending = deferred<DocumentPipWindowLike>();
		const pipWindow = new FakePipWindow();
		const { api, request } = fakeApi(() => pending.promise);
		const onOpen = vi.fn();
		const controller = createDocumentPipController({ host: fakeHost(api), onOpen });

		const opening = controller.toggle();
		const cancelling = controller.toggle();
		pending.resolve(pipWindow);
		await expect(cancelling).resolves.toBe(false);
		await expect(opening).resolves.toBe(false);
		expect(request).toHaveBeenCalledOnce();
		expect(pipWindow.closeCount).toBe(1);
		expect(onOpen).not.toHaveBeenCalled();
		expect(controller.isOpen()).toBe(false);
	});

	it('a rejection arriving after dispose is inert (no error callback)', async () => {
		const pending = deferred<DocumentPipWindowLike>();
		const { api } = fakeApi(() => pending.promise);
		const onError = vi.fn();
		const controller = createDocumentPipController({ host: fakeHost(api), onError });

		const opening = controller.open();
		controller.dispose();
		pending.reject(new Error('late rejection'));
		await expect(opening).resolves.toBe(false);
		expect(onError).not.toHaveBeenCalled();
	});

	it('treats a throwing onOpen consumer as an open failure and closes the window', async () => {
		const { api, pipWindow } = fakeApi();
		const mountFailure = new Error('mount failed');
		const onError = vi.fn();
		const controller = createDocumentPipController({
			host: fakeHost(api),
			onOpen: () => {
				throw mountFailure;
			},
			onError
		});

		await expect(controller.open()).resolves.toBe(false);
		expect(pipWindow.closeCount).toBe(1);
		expect(controller.isOpen()).toBe(false);
		expect(controller.currentWindow()).toBeNull();
		expect(onError).toHaveBeenCalledOnce();
		expect(onError).toHaveBeenCalledWith(mountFailure);
	});

	it('a throwing onClose consumer cannot stop the window closing', async () => {
		const { api, pipWindow } = fakeApi();
		const controller = createDocumentPipController({
			host: fakeHost(api),
			onClose: () => {
				throw new Error('teardown failed');
			}
		});
		await controller.open();

		expect(() => controller.close()).not.toThrow();
		expect(pipWindow.closeCount).toBe(1);
		expect(controller.isOpen()).toBe(false);
	});

	it('PIP-05: the controller source has no pause-like or playback coupling', () => {
		// Raw source via Vite glob: this package has no node types.
		const sources = import.meta.glob('./documentPip.ts', {
			query: '?raw',
			import: 'default',
			eager: true
		}) as Record<string, string>;
		const source = Object.values(sources)[0] ?? '';
		expect(source).toContain('createDocumentPipController');
		expect(source).not.toMatch(/\bpause\s*\(/);
		expect(source).not.toMatch(/from\s+['"]\.\/playback(\.js)?['"]/);
		expect(source).not.toMatch(/AudioLike|HTMLAudioElement/);
	});
});
