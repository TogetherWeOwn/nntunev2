import { onDestroy, onMount } from 'svelte';
import { after, before, handles, navigate } from './route.svelte';

// One-time authorized ownership migration: only the router is absent from this
// component fixture; the actual layout/session/audio/library remain connected.
// General protected-fixture restrictions still apply outside that exception.
export function afterNavigate(callback: () => void): void {
	after.add(callback);
	onDestroy(() => { after.delete(callback); });
	onMount(() => { callback(); });
}

export function beforeNavigate(callback: () => void): void {
	before.add(callback);
	onDestroy(() => { before.delete(callback); });
	onMount(() => {
		window.addEventListener('beforeunload', callback);
		return () => window.removeEventListener('beforeunload', callback);
	});
}

export async function goto(path: string): Promise<void> {
	if (handles(path)) return navigate(path, { type: 'goto' });
	window.location.assign(path);
}
