<script lang="ts">
	import { onMount } from 'svelte';
	import { on } from 'svelte/events';
	import { page } from './state';
	import { handles, navigate, onPop } from './route.svelte';
	import NowPlaying from '../../../src/routes/now-playing/+page.svelte';
	import Layout from '../../../src/routes/+layout.svelte';
	import Library from '../../../src/routes/library/+page.svelte';
	import type { QueueState } from '../../../src/lib/player/queue.js';

	// Engineering Lead authorized a one-time same-PR migration from library-local
	// native controls to the approved persistent custom transport. The bounded
	// executor exception mounts the real layout/child and exports queue input;
	// all other Builder edits remain prohibited. No parallel player is mounted.
	let library: { acceptanceReplaceQueue(next: QueueState): void } | undefined;
	let root: HTMLDivElement;
	export function acceptanceReplaceQueue(next: QueueState): boolean {
		if (!library) return false;
		library.acceptanceReplaceQueue(next);
		return true;
	}

	onMount(() => {
		function onClick(event: MouseEvent): void {
			if (event.defaultPrevented || event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
			const anchor = event.target instanceof Element ? event.target.closest('a[href]') : null;
			if (!(anchor instanceof HTMLAnchorElement) || anchor.hasAttribute('target') || anchor.hasAttribute('download') || !handles(anchor.href)) return;
			event.preventDefault();
			void navigate(anchor.href, { type: 'link', replace: anchor.hasAttribute('data-sveltekit-replacestate') });
		}
		const removeClick = on(root, 'click', onClick);
		window.addEventListener('popstate', onPop);
		return () => {
			removeClick();
			window.removeEventListener('popstate', onPop);
		};
	});
</script>

<div bind:this={root}>
	<Layout>
		{#if page.url.pathname === '/library'}
			<Library bind:this={library} />
		{:else if page.url.pathname === '/now-playing'}
			<NowPlaying />
		{/if}
	</Layout>
</div>
