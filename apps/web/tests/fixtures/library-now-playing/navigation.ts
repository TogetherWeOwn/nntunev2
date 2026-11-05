import { onMount } from 'svelte';

// One-time authorized ownership migration: only the router is absent from this
// component fixture; the actual layout/session/audio/library remain connected.
// General protected-fixture restrictions still apply outside that exception.
export function afterNavigate(callback: () => void): void {
	onMount(() => { callback(); });
}

export function beforeNavigate(callback: () => void): void {
	onMount(() => {
		window.addEventListener('beforeunload', callback);
		return () => window.removeEventListener('beforeunload', callback);
	});
}

export async function goto(path: string): Promise<void> {
	window.location.assign(path);
}
