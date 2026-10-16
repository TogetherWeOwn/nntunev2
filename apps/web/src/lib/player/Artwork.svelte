<script lang="ts">
	import {
		ARTWORK_PLACEHOLDER_SRC,
		nextArtworkSrcAfterError,
		resetFailedArtworkSrcOnChange,
		resolveArtwork
	} from './artwork.js';

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
	let lastResolvedSrc = $state<string | null>(null);
	let shownSrc = $derived(failedSrc === resolved.src ? fallbackSrc : resolved.src);

	$effect(() => {
		const currentSrc = resolved.src;
		if (lastResolvedSrc !== null) {
			const nextFailedSrc = resetFailedArtworkSrcOnChange(lastResolvedSrc, currentSrc, failedSrc);
			if (nextFailedSrc !== failedSrc) failedSrc = nextFailedSrc;
		}
		lastResolvedSrc = currentSrc;
	});

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
