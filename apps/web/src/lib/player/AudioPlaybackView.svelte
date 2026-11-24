<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { Track } from './queue.js';
	let { children }: { track: Track; children: Snippet } = $props();
</script>
{@render children()}
