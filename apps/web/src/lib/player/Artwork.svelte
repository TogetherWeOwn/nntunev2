<script lang="ts">
	import { ARTWORK_PLACEHOLDER_SRC, nextArtworkSrcAfterError, resolveArtwork } from './artwork.js';

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
	let failedSrc = $state<string | null>(null);
	let shownSrc = $derived(failedSrc === resolved.src ? fallbackSrc : resolved.src);

	function handleError(): void {
		// Stops once the placeholder itself fails: no loop, no broken-image state.
		if (nextArtworkSrcAfterError(shownSrc, fallbackSrc) === null) return;
		failedSrc = resolved.src;
	}
</script>

<img
	src={shownSrc}
	alt={resolved.alt}
	class={className}
	data-fallback={shownSrc === fallbackSrc ? 'true' : 'false'}
	data-testid="track-artwork"
	onerror={handleError}
/>
