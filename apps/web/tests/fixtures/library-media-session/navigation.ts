import { onMount } from 'svelte';

// This standalone fixture has no router; initial navigation completes on mount.
export function afterNavigate(callback: () => void): void {
	onMount(() => { callback(); });
}
