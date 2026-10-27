import { onMount } from 'svelte';

// The standalone real-page mount has no Kit router; mount completes navigation.
export function afterNavigate(callback: () => void): void {
	onMount(callback);
}
