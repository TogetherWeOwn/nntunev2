import { tick } from 'svelte';

let url = $state(new URL('/library', location.origin));
let idx = $state(0);

type Navigation = {
	from: { url: URL };
	to: { url: URL };
	type: 'goto' | 'link' | 'popstate';
	willUnload: false;
	cancel(): void;
};
export const before = new Set<(navigation: Navigation) => void>();
export const after = new Set<(navigation: Navigation) => void>();
export const route = { get url() { return url; } };

export function handles(path: string): boolean {
	const target = new URL(path, location.origin);
	return target.origin === location.origin && ['/library', '/now-playing'].includes(target.pathname);
}

export async function navigate(path: string, options: { type: Navigation['type']; replace?: boolean; index?: number }): Promise<void> {
	if (!handles(path)) return;
	const target = new URL(path, location.origin);
	let cancelled = false;
	const navigation: Navigation = {
		from: { url }, to: { url: target }, type: options.type, willUnload: false,
		cancel() { cancelled = true; }
	};
	for (const callback of before) callback(navigation);
	if (cancelled && options.type !== 'popstate') return;
	if (options.type === 'popstate') {
		idx = options.index ?? 0;
	} else if (options.replace) {
		history.replaceState({ fixtureRoute: path, idx }, '', path);
	} else {
		history.pushState({ fixtureRoute: path, idx: ++idx }, '', path);
	}
	url = target;
	await tick();
	for (const callback of after) callback(navigation);
}

export function onPop(event: PopStateEvent): void {
	const path = event.state?.fixtureRoute;
	void navigate(typeof path === 'string' && handles(path) ? path : '/library', {
		type: 'popstate', index: typeof event.state?.idx === 'number' ? event.state.idx : 0
	});
}
