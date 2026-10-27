<script lang="ts">
	import { HOME_SUMMARY } from '#lib/publicCopy.js';
</script>

<svelte:head>
	<title>NNTune — your music, your provider</title>
	<meta name="description" content={HOME_SUMMARY} />
</svelte:head>

<main class="nt-wrap" data-testid="app-root">
	<h1>NNTune</h1>
	<p>{HOME_SUMMARY}</p>
	<h2>Bring your own provider (BYOP)</h2>
	<p>
		Bring your own Usenet provider account — NNTune never bundles or resells Usenet access.
	</p>
	<ol>
		<li>Try the demo player without an account.</li>
		<li>Create an account to get started with NNTune.</li>
		<li>Connect your own Usenet provider in account settings.</li>
	</ol>
	<p>
		<a class="nt-button" href="/demo/player">Try the demo player</a>
		<a class="nt-button" href="/signup">Get started</a>
		<a class="nt-button nt-button--ghost" href="/pricing">See plans</a>
	</p>
</main>
