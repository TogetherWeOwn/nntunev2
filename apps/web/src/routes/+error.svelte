<script lang="ts">
	import { page } from '$app/state';

	const status = $derived(page.status);
	const isNotFound = $derived(status === 404);
</script>

<svelte:head>
	<title>NNTune — Page not found</title>
	<meta
		name="description"
		content="This NNTune page does not exist. Go back to your library or sign in."
	/>
</svelte:head>

<main class="nt-wrap" data-testid="error-page">
	<h1>{isNotFound ? '404 — Page not found' : `${status} — Something went wrong`}</h1>
	<p>
		NNTune could not find this page. Head back to your library to keep listening, or sign in
		to continue.
	</p>
	<p>
		<a class="nt-button" href="/library">Back to your library</a>
		<a class="nt-button nt-button--ghost" href="/login">Sign in</a>
	</p>
</main>
