export interface FullscreenSnapshot {
	supported: boolean;
	active: boolean;
	pending: boolean;
	failed: boolean;
}

/** One presentation owns one element; never exit another feature's full screen. */
export function createFullscreenController(
	element: HTMLElement,
	onChange: (state: FullscreenSnapshot) => void
) {
	const document = element.ownerDocument;
	let disposed = false;
	let pending = false;
	let failed = false;

	function snapshot(): FullscreenSnapshot {
		return {
			supported: document.fullscreenEnabled !== false &&
				typeof element.requestFullscreen === 'function' && typeof document.exitFullscreen === 'function',
			active: document.fullscreenElement === element,
			pending,
			failed
		};
	}

	function publish(): void {
		if (!disposed) onChange(snapshot());
	}

	async function release(): Promise<void> {
		if (document.fullscreenElement !== element) return;
		try {
			await document.exitFullscreen();
		} catch {
			// Teardown cannot claim an exit the browser refused, or affect playback.
		}
	}

	async function change(enter: boolean): Promise<boolean> {
		if (disposed || pending || !element.isConnected) return false;
		const state = snapshot();
		if (enter === state.active) return true;
		if (enter && (!state.supported || document.fullscreenElement)) return false;
		pending = true;
		failed = false;
		publish();
		let completed = false;
		try {
			// A subscriber may synchronously retire or replace the presentation.
			if (disposed || !element.isConnected) return false;
			if (enter) {
				if (!snapshot().supported || (document.fullscreenElement && document.fullscreenElement !== element)) return false;
				if (document.fullscreenElement !== element) await element.requestFullscreen();
			} else if (document.fullscreenElement === element) {
				await document.exitFullscreen();
			}
			if (disposed || !element.isConnected) {
				await release();
				return false;
			}
			completed = true;
		} catch {
			if (!disposed) failed = true;
		} finally {
			pending = false;
			publish();
		}
		return completed && !disposed && element.isConnected && snapshot().active === enter;
	}

	function onChangeEvent(): void {
		failed = false;
		publish();
	}

	document.addEventListener('fullscreenchange', onChangeEvent);
	publish();

	return {
		snapshot,
		enter: () => change(true),
		exit: () => change(false),
		toggle: () => change(!snapshot().active),
		destroy(): void {
			if (disposed) return;
			disposed = true;
			document.removeEventListener('fullscreenchange', onChangeEvent);
			void release();
		}
	};
}
