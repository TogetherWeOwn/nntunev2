<script lang="ts">
	import { page } from '$app/state';

	const status = $derived(page.status);
	const isNotFound = $derived(status === 404);
</script>

<svelte:head>
	<title>{isNotFound ? 'NNTune — Page not found' : 'NNTune — Something went wrong'}</title>
	<meta
		name="description"
		content={isNotFound
			? 'This NNTune page does not exist. Go back to your library or sign in.'
			: 'Something went wrong on our side. Try again, or head back to your library.'}
	/>
</svelte:head>

<main class="nt-wrap" data-testid="error-page">
	<h1>{isNotFound ? '404 — Page not found' : `${status} — Something went wrong`}</h1>
	<p role={isNotFound ? undefined : 'alert'}>
		{isNotFound
			? 'NNTune could not find this page. Head back to your library to keep listening, or sign in to continue.'
			: 'NNTune hit a problem loading this page. Try again, or head back to your library to keep listening.'}
	</p>
	<p>
		{#if !isNotFound}
			<button class="nt-button" type="button" onclick={() => window.location.reload()}>Reload</button>
		{/if}
		<a class="nt-button" href="/library">Back to your library</a>
		<a class="nt-button nt-button--ghost" href="/login">Sign in</a>
	</p>
</main>

<style>
	button {
		font: inherit;
		border: 0;
		cursor: pointer;
	}
</style>
