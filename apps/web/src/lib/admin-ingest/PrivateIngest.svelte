<script lang="ts">
	import IngestScreens from './IngestScreens.svelte';
	import type { IngestRequests } from './requests.js';

	let { enabled = false, role, requests }: {
		enabled?: boolean;
		role?: 'owner' | 'member' | 'anonymous';
		requests?: IngestRequests;
	} = $props();
	let suspended = $state(false);
</script>

<svelte:window onpagehide={() => { suspended = true; }} />

{#if enabled === true && role === 'owner' && requests && !suspended}
	{#key requests}
		<IngestScreens {requests} />
	{/key}
{:else}
	<main class="nt-wrap">
		<h1>Page not found</h1>
	</main>
{/if}
