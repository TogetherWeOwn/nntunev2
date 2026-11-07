import { afterEach, expect, it, vi } from 'vitest';
import { createCinemaController, type CinemaSnapshot } from './cinema.js';

const controllers: ReturnType<typeof createCinemaController>[] = [];

function fixture(supported = true) {
	vi.useFakeTimers();
	const document = Object.assign(new EventTarget(), {
		fullscreenEnabled: supported,
		fullscreenElement: null as HTMLElement | null,
		exitFullscreen: vi.fn(async () => setActive(null))
	});
	const element = Object.assign(new EventTarget(), {
		ownerDocument: document,
		isConnected: true,
		requestFullscreen: vi.fn(async () => setActive(element as unknown as HTMLElement))
	});
	function setActive(active: HTMLElement | null) {
		document.fullscreenElement = active;
		document.dispatchEvent(new Event('fullscreenchange'));
	}
	const states: CinemaSnapshot[] = [];
	const controller = createCinemaController(element as unknown as HTMLElement, (state) => states.push(state));
	controllers.push(controller);
	return { controller, states, element, document, setActive };
}

afterEach(() => {
	controllers.splice(0).forEach((controller) => controller.destroy());
	vi.useRealTimers();
});

it('has no idle timer until its own native full screen is active', async () => {
	const { controller, states, element } = fixture();
	expect(states.at(-1)).toEqual({ supported: true, active: false, pending: false, failed: false, idle: false });
	vi.advanceTimersByTime(10000);
	expect(vi.getTimerCount()).toBe(0);
	expect(states.at(-1)?.idle).toBe(false);
	await controller.toggle();
	expect(element.requestFullscreen).toHaveBeenCalledOnce();
	vi.advanceTimersByTime(2999);
	expect(states.at(-1)?.idle).toBe(false);
	vi.advanceTimersByTime(1);
	expect(states.at(-1)?.idle).toBe(true);
	expect(vi.getTimerCount()).toBe(0);
});

it.each(['pointermove', 'pointerdown', 'keydown', 'focusin'])('%s reveals controls and starts one fresh idle deadline', async (event) => {
	const { controller, states, element } = fixture();
	await controller.toggle();
	vi.advanceTimersByTime(3000);
	element.dispatchEvent(new Event(event));
	expect(states.at(-1)?.idle).toBe(false);
	vi.advanceTimersByTime(2000);
	element.dispatchEvent(new Event(event));
	expect(vi.getTimerCount()).toBe(1);
	vi.advanceTimersByTime(2999);
	expect(states.at(-1)?.idle).toBe(false);
	vi.advanceTimersByTime(1);
	expect(states.at(-1)?.idle).toBe(true);
});

it.each(['escape', 'toggle', 'foreign'] as const)('%s retires idle state and its pending timer', async (exit) => {
	const { controller, states, document, setActive } = fixture();
	await controller.toggle();
	vi.advanceTimersByTime(3000);
	if (exit === 'toggle') await controller.toggle();
	else setActive(exit === 'foreign' ? {} as HTMLElement : null);
	expect(states.at(-1)).toMatchObject({ active: false, idle: false });
	expect(vi.getTimerCount()).toBe(0);
	if (exit === 'foreign') {
		expect(await controller.toggle()).toBe(false);
		expect(document.exitFullscreen).not.toHaveBeenCalled();
	}
});

it('unsupported native full screen never starts a timer or requests entry', async () => {
	const { controller, states, element } = fixture(false);
	expect(await controller.toggle()).toBe(false);
	expect(states.at(-1)).toMatchObject({ supported: false, active: false, idle: false });
	expect(element.requestFullscreen).not.toHaveBeenCalled();
	expect(vi.getTimerCount()).toBe(0);
});

it('a refused entry is exposed without hiding the controls and a later retry can enter', async () => {
	const { controller, states, element, setActive } = fixture();
	element.requestFullscreen.mockRejectedValueOnce(new Error('Refused'));
	expect(await controller.toggle()).toBe(false);
	expect(states.at(-1)).toMatchObject({ failed: true, active: false, idle: false });
	expect(vi.getTimerCount()).toBe(0);
	element.requestFullscreen.mockImplementation(async () => setActive(element as unknown as HTMLElement));
	expect(await controller.toggle()).toBe(true);
	expect(states.at(-1)).toMatchObject({ failed: false, active: true, idle: false });
});

it('destroy removes listeners, cancels the timer, exits only its element and never publishes later', async () => {
	const { controller, states, element, document, setActive } = fixture();
	await controller.toggle();
	controller.destroy();
	const count = states.length;
	expect(document.exitFullscreen).toHaveBeenCalledOnce();
	expect(vi.getTimerCount()).toBe(0);
	for (const event of ['pointermove', 'pointerdown', 'keydown', 'focusin']) element.dispatchEvent(new Event(event));
	setActive(null);
	vi.advanceTimersByTime(10000);
	expect(states).toHaveLength(count);
	expect(await controller.toggle()).toBe(false);
	controller.destroy();
	expect(document.exitFullscreen).toHaveBeenCalledOnce();
});

it('destroy during an unresolved native entry fences a late success and leaves no timer', async () => {
	const { controller, states, element, document, setActive } = fixture();
	let resolve!: () => void;
	element.requestFullscreen.mockImplementation(() => new Promise<void>((yes) => { resolve = yes; }));
	const pending = controller.toggle();
	controller.destroy();
	const count = states.length;
	setActive(element as unknown as HTMLElement);
	resolve();
	expect(await pending).toBe(false);
	expect(document.exitFullscreen).toHaveBeenCalledOnce();
	expect(vi.getTimerCount()).toBe(0);
	expect(states).toHaveLength(count);
});
