import { mount } from 'svelte';
import Probe from './Probe.svelte';

type ProbeApi = {
	requestWindow(options: { width: number; height: number }): Promise<Window>;
};

const w = window as unknown as {
	documentPictureInPicture?: ProbeApi;
	__probe: Record<string, unknown>;
	__pipWindow?: Window;
};

w.__probe = {
	isSecureContext: window.isSecureContext,
	hasApi: 'documentPictureInPicture' in window,
	opened: false,
	pagehideFired: false
};

document.querySelector('#open')?.addEventListener('click', async () => {
	try {
		if (!w.documentPictureInPicture) throw new Error('api-missing');
		const pipWindow = await w.documentPictureInPicture.requestWindow({ width: 400, height: 240 });
		w.__pipWindow = pipWindow;
		w.__probe.opened = true;

		// Clone same-origin readable styles, the way the build slice would.
		let clonedRules = 0;
		for (const sheet of Array.from(document.styleSheets)) {
			try {
				const text = Array.from(sheet.cssRules)
					.map((rule) => rule.cssText)
					.join('\n');
				const style = pipWindow.document.createElement('style');
				style.textContent = text;
				pipWindow.document.head.append(style);
				clonedRules += sheet.cssRules.length;
			} catch {
				// Unreadable (cross-origin) sheets are skipped, never fetched.
			}
		}
		w.__probe.clonedRules = clonedRules;

		mount(Probe, { target: pipWindow.document.body, props: { label: 'Probe mounted' } });
		const text = pipWindow.document.body.querySelector('.probe-text');
		w.__probe.mountedText = text?.textContent ?? null;
		w.__probe.computedColor = text ? pipWindow.getComputedStyle(text).color : null;
		w.__probe.pipTitleSet = (() => {
			pipWindow.document.title = 'Mini-player';
			return pipWindow.document.title;
		})();

		pipWindow.addEventListener('pagehide', () => {
			w.__probe.pagehideFired = true;
		});
	} catch (error) {
		w.__probe.errorName = error instanceof Error ? error.name : typeof error;
		w.__probe.errorMessage = error instanceof Error ? error.message : String(error);
	}
});
