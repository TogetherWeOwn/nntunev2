import { onMount } from 'svelte';

// The standalone real-page mount has no Kit router; mount completes navigation.
export function afterNavigate(callback: () => void): void {
	onMount(callback);
}

// Import/lifecycle adapter only: this standalone mount navigates by unloading.
export function beforeNavigate(callback: () => void): void {
	onMount(() => {
		window.addEventListener('beforeunload', callback);
		return () => window.removeEventListener('beforeunload', callback);
	});
}
