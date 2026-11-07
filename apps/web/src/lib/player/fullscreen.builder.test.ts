import { describe, expect, it, vi } from 'vitest';
import { createFullscreenController, type FullscreenSnapshot } from './fullscreen.js';

function deferred() {
	let resolve!: () => void;
	let reject!: (error: Error) => void;
	const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no; });
	return { promise, resolve, reject };
}

function fixture() {
	const events = new EventTarget();
	const document = Object.assign(events, {
		fullscreenEnabled: true,
		fullscreenElement: null as HTMLElement | null,
		exitFullscreen: vi.fn(async () => setActive(null))
	});
	const element = {
		ownerDocument: document,
		isConnected: true,
		requestFullscreen: vi.fn(async () => setActive(element as unknown as HTMLElement))
	};
	function setActive(active: HTMLElement | null) {
		document.fullscreenElement = active;
		document.dispatchEvent(new Event('fullscreenchange'));
	}
	const states: FullscreenSnapshot[] = [];
	const controller = createFullscreenController(element as unknown as HTMLElement, (state) => states.push(state));
	return { controller, element, document, states, setActive };
}

describe('native full-screen presentation lifecycle', () => {
	it('starts from native state and enters and exits only its element', async () => {
		const { controller, element, document, states } = fixture();
		expect(states).toEqual([{ supported: true, active: false, pending: false, failed: false }]);
		expect(await controller.enter()).toBe(true);
		expect(element.requestFullscreen).toHaveBeenCalledOnce();
		expect(controller.snapshot()).toEqual({ supported: true, active: true, pending: false, failed: false });
		expect(await controller.enter()).toBe(true);
		expect(element.requestFullscreen).toHaveBeenCalledOnce();
		expect(await controller.toggle()).toBe(true);
		expect(document.exitFullscreen).toHaveBeenCalledOnce();
		expect(controller.snapshot().active).toBe(false);
		controller.destroy();
	});

	it('tracks native Escape and browser chrome exits without issuing another exit', async () => {
		const { controller, document, states, setActive } = fixture();
		await controller.enter();
		setActive(null);
		expect(states.at(-1)).toEqual({ supported: true, active: false, pending: false, failed: false });
		expect(document.exitFullscreen).not.toHaveBeenCalled();
		expect(await controller.exit()).toBe(true);
		expect(document.exitFullscreen).not.toHaveBeenCalled();
		controller.destroy();
	});

	it.each(['disabled', 'missing-request', 'missing-exit'] as const)('does not enter when unsupported: %s', async (kind) => {
		const { controller, element, document } = fixture();
		if (kind === 'disabled') document.fullscreenEnabled = false;
		if (kind === 'missing-request') Object.assign(element, { requestFullscreen: undefined });
		if (kind === 'missing-exit') Object.assign(document, { exitFullscreen: undefined });
		expect(controller.snapshot().supported).toBe(false);
		expect(await controller.enter()).toBe(false);
		expect(controller.snapshot().active).toBe(false);
		controller.destroy();
	});

	it('does not steal, exit or clean up a different feature full screen', async () => {
		const { controller, element, document, setActive } = fixture();
		const other = {} as HTMLElement;
		setActive(other);
		expect(await controller.enter()).toBe(false);
		expect(await controller.toggle()).toBe(false);
		expect(await controller.exit()).toBe(true);
		controller.destroy();
		expect(document.fullscreenElement).toBe(other);
		expect(element.requestFullscreen).not.toHaveBeenCalled();
		expect(document.exitFullscreen).not.toHaveBeenCalled();
	});

	it.each(['throw', 'reject'] as const)('reflects a refused request without assuming entry: %s', async (kind) => {
		const { controller, element, states } = fixture();
		if (kind === 'throw') element.requestFullscreen.mockImplementation(() => { throw new Error('Denied'); });
		else element.requestFullscreen.mockRejectedValue(new Error('Denied'));
		expect(await controller.enter()).toBe(false);
		expect(states.at(-1)).toEqual({ supported: true, active: false, pending: false, failed: true });
		element.requestFullscreen.mockImplementation(async () => {
			element.ownerDocument.fullscreenElement = element as unknown as HTMLElement;
		});
		expect(await controller.enter()).toBe(true);
		expect(controller.snapshot().failed).toBe(false);
		controller.destroy();
	});

	it('keeps native active state when an exit is refused', async () => {
		const { controller, document, states } = fixture();
		await controller.enter();
		document.exitFullscreen.mockRejectedValue(new Error('Denied'));
		expect(await controller.exit()).toBe(false);
		expect(states.at(-1)).toEqual({ supported: true, active: true, pending: false, failed: true });
		controller.destroy();
		await Promise.resolve();
	});

	it('does not claim entry or exit just because a browser promise resolved', async () => {
		const { controller, element, document, setActive } = fixture();
		element.requestFullscreen.mockResolvedValue(undefined);
		expect(await controller.enter()).toBe(false);
		setActive(element as unknown as HTMLElement);
		document.exitFullscreen.mockResolvedValue(undefined);
		expect(await controller.exit()).toBe(false);
		expect(controller.snapshot().active).toBe(true);
		controller.destroy();
	});

	it('deduplicates rapid actions while a request is pending', async () => {
		const { controller, element, setActive } = fixture();
		const gate = deferred();
		element.requestFullscreen.mockImplementation(() => gate.promise);
		const first = controller.enter();
		expect(controller.snapshot().pending).toBe(true);
		expect(await controller.enter()).toBe(false);
		expect(await controller.toggle()).toBe(false);
		expect(await controller.exit()).toBe(false);
		expect(element.requestFullscreen).toHaveBeenCalledOnce();
		setActive(element as unknown as HTMLElement);
		gate.resolve();
		expect(await first).toBe(true);
		expect(controller.snapshot().pending).toBe(false);
		controller.destroy();
	});

	it('releases a late successful request after teardown without notifying retired UI', async () => {
		const { controller, element, document, states, setActive } = fixture();
		const gate = deferred();
		element.requestFullscreen.mockImplementation(() => gate.promise);
		const result = controller.enter();
		controller.destroy();
		const count = states.length;
		setActive(element as unknown as HTMLElement);
		gate.resolve();
		expect(await result).toBe(false);
		expect(document.exitFullscreen).toHaveBeenCalledOnce();
		expect(document.fullscreenElement).toBeNull();
		expect(states).toHaveLength(count);
		expect(await controller.toggle()).toBe(false);
	});

	it('does not exit another feature when a retired request settles', async () => {
		const { controller, element, document, setActive } = fixture();
		const gate = deferred();
		element.requestFullscreen.mockImplementation(() => gate.promise);
		const result = controller.enter();
		controller.destroy();
		const other = {} as HTMLElement;
		setActive(other);
		gate.resolve();
		expect(await result).toBe(false);
		expect(document.fullscreenElement).toBe(other);
		expect(document.exitFullscreen).not.toHaveBeenCalled();
	});

	it('absorbs a late rejected request after teardown', async () => {
		const { controller, element, states } = fixture();
		const gate = deferred();
		element.requestFullscreen.mockImplementation(() => gate.promise);
		const result = controller.enter();
		controller.destroy();
		const count = states.length;
		gate.reject(new Error('Denied'));
		expect(await result).toBe(false);
		expect(states).toHaveLength(count);
	});

	it('removes listeners and exits its active presentation once on teardown', async () => {
		const { controller, document, states, setActive } = fixture();
		const remove = vi.spyOn(document, 'removeEventListener');
		await controller.enter();
		controller.destroy();
		controller.destroy();
		const count = states.length;
		setActive(null);
		expect(remove).toHaveBeenCalledOnce();
		expect(document.exitFullscreen).toHaveBeenCalledOnce();
		expect(states).toHaveLength(count);
	});

	it('refuses disconnected elements, including a disconnection during the request', async () => {
		const { controller, element, document, setActive } = fixture();
		element.isConnected = false;
		expect(await controller.enter()).toBe(false);
		expect(element.requestFullscreen).not.toHaveBeenCalled();
		element.isConnected = true;
		const gate = deferred();
		element.requestFullscreen.mockImplementation(() => gate.promise);
		const result = controller.enter();
		element.isConnected = false;
		setActive(element as unknown as HTMLElement);
		gate.resolve();
		expect(await result).toBe(false);
		expect(document.exitFullscreen).toHaveBeenCalledOnce();
		controller.destroy();
	});

	it('allows exiting an owned presentation even if entry support becomes disabled', async () => {
		const { controller, document } = fixture();
		await controller.enter();
		document.fullscreenEnabled = false;
		expect(await controller.exit()).toBe(true);
		expect(document.exitFullscreen).toHaveBeenCalledOnce();
		controller.destroy();
	});

	it.each(['destroy', 'replace'] as const)('does not report stale entry after the completion subscriber chooses to %s', async (kind) => {
		const { element, document, controller: initial, setActive } = fixture();
		initial.destroy();
		let pendingSeen = false;
		let retire = () => {};
		const controller = createFullscreenController(element as unknown as HTMLElement, (state) => {
			if (state.pending) pendingSeen = true;
			else if (pendingSeen) {
				pendingSeen = false;
				retire();
			}
		});
		const other = {} as HTMLElement;
		retire = kind === 'destroy' ? () => controller.destroy() : () => setActive(other);
		expect(await controller.enter()).toBe(false);
		if (kind === 'replace') {
			expect(document.fullscreenElement).toBe(other);
			expect(document.exitFullscreen).not.toHaveBeenCalled();
		}
		controller.destroy();
	});

	it.each(['enter', 'exit'] as const)('does not disturb a subscriber-selected foreign presentation during %s', async (action) => {
		const { element, document, controller: initial, setActive } = fixture();
		initial.destroy();
		if (action === 'exit') setActive(element as unknown as HTMLElement);
		const other = {} as HTMLElement;
		const controller = createFullscreenController(element as unknown as HTMLElement, (state) => {
			if (state.pending && document.fullscreenElement !== other) setActive(other);
		});
		expect(await controller[action]()).toBe(action === 'exit');
		expect(document.fullscreenElement).toBe(other);
		expect(element.requestFullscreen).not.toHaveBeenCalled();
		expect(document.exitFullscreen).not.toHaveBeenCalled();
		controller.destroy();
	});

	it.each(['destroy', 'disconnect'] as const)('does not enter after a pending-state subscriber retires the surface: %s', async (kind) => {
		const { element, controller: initial } = fixture();
		initial.destroy();
		let retire = () => {};
		const controller = createFullscreenController(element as unknown as HTMLElement, (state) => {
			if (state.pending) retire();
		});
		retire = kind === 'destroy' ? () => controller.destroy() : () => { element.isConnected = false; };
		expect(await controller.enter()).toBe(false);
		expect(element.requestFullscreen).not.toHaveBeenCalled();
		expect(controller.snapshot().pending).toBe(false);
		controller.destroy();
	});
});
