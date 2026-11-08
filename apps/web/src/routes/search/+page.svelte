<script lang="ts">
	import { page } from '$app/state';
	import SearchPanel from '#lib/search/SearchPanel.svelte';
	import { searchCategory } from '#lib/search/results.js';
</script>

<svelte:head><title>Search — NNTune</title></svelte:head>

<main class="nt-wrap" id="main-content">
	<h1>Search</h1>
	<SearchPanel query={page.url.searchParams.get('q') ?? ''} category={searchCategory(page.url.searchParams.get('type'))} />
</main>
