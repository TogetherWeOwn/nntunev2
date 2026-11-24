<script lang="ts">
	import IngestScreens from './IngestScreens.svelte';
	import type { IngestRequests } from './requests.js';

	let { enabled = false, role, requests }: {
		enabled?: boolean;
		role?: 'owner' | 'member' | 'anonymous';
		requests?: IngestRequests;
	} = $props();
	let authorizedRequests = $derived(enabled === true && role === 'owner' ? requests : undefined);
	let suspended = $state(false);

	function onPageShow(event: PageTransitionEvent) {
		// Enter through a fresh authorization check after a cached restore.
		if (event.persisted && suspended) window.location.reload();
	}
</script>

<svelte:window onpagehide={() => { suspended = !!authorizedRequests; }} onpageshow={onPageShow} />

{#if authorizedRequests && !suspended}
	{#key authorizedRequests}
		<IngestScreens requests={authorizedRequests} />
	{/key}
{:else}
	<main class="nt-wrap">
		<h1>Page not found</h1>
	</main>
{/if}
