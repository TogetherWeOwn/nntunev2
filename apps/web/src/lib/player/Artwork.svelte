<script lang="ts">
	import { ARTWORK_PLACEHOLDER_SRC, resolveArtwork } from './artwork.js';

	interface Props {
		src?: string | null;
		alt?: string;
		fallbackSrc?: string;
		class?: string;
	}

	let {
		src = null,
		alt = '',
		fallbackSrc = ARTWORK_PLACEHOLDER_SRC,
		class: className = ''
	}: Props = $props();

	let resolved = $derived(resolveArtwork(src, { fallbackSrc, alt }));

	function handleError(event: Event): void {
		const img = event.currentTarget as HTMLImageElement;
		if (img.dataset['fallback'] === 'true' || img.onerror === null) return;
		// Swap to the placeholder exactly once; then stop so a missing
		// placeholder can never loop or leave a broken-image icon.
		img.dataset['fallback'] = 'true';
		img.onerror = null;
		img.src = fallbackSrc;
	}
</script>

<img
	src={resolved.src}
	alt={resolved.alt}
	class={className}
	data-fallback={resolved.isFallback ? 'true' : 'false'}
	data-testid="track-artwork"
	onerror={handleError}
/>
