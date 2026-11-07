import { createFullscreenController, type FullscreenSnapshot } from './fullscreen.js';

export interface CinemaSnapshot extends FullscreenSnapshot {
	idle: boolean;
}

/** Presentation only: the persistent audio element and transport keep their owner. */
export function createCinemaController(element: HTMLElement, onChange: (state: CinemaSnapshot) => void) {
	let disposed = false;
	let idle = false;
	let timer: ReturnType<typeof setTimeout> | undefined;
	let state: FullscreenSnapshot;

	function clearTimer(): void {
		if (timer !== undefined) clearTimeout(timer);
		timer = undefined;
	}

	function publish(): void {
		if (!disposed) onChange({ ...state, idle });
	}

	function wake(): void {
		if (disposed) return;
		clearTimer();
		idle = false;
		if (state.active) {
			timer = setTimeout(() => {
				timer = undefined;
				idle = true;
				publish();
			}, 3000);
		}
		publish();
	}

	const fullscreen = createFullscreenController(element, (next) => {
		state = next;
		wake();
	});
	const events = ['pointermove', 'pointerdown', 'keydown', 'focusin'] as const;
	for (const event of events) element.addEventListener(event, wake);

	return {
		toggle: fullscreen.toggle,
		exit: fullscreen.exit,
		destroy(): void {
			if (disposed) return;
			disposed = true;
			clearTimer();
			for (const event of events) element.removeEventListener(event, wake);
			fullscreen.destroy();
		}
	};
}
