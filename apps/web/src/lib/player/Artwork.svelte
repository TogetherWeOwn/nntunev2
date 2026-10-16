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
	let fallbackFailed = $state(false);
	let lastResolvedSrc = $state<string | null>(null);
	let shownSrc = $derived(failedSrc === resolved.src ? fallbackSrc : resolved.src);

	$effect(() => {
		const currentSrc = resolved.src;
		if (lastResolvedSrc !== null && lastResolvedSrc !== currentSrc) {
			failedSrc = resetFailedArtworkSrcOnChange(lastResolvedSrc, currentSrc, failedSrc);
			fallbackFailed = false;
		}
		lastResolvedSrc = currentSrc;
	});

	function handleError(): void {
		if (nextArtworkSrcAfterError(shownSrc, fallbackSrc) === null) {
			fallbackFailed = true;
			return;
		}
		failedSrc = resolved.src;
	}
</script>

{#if fallbackFailed}
	<span class={className} role="img" aria-label={resolved.alt} data-testid="track-artwork-fallback">
		{resolved.alt}
	</span>
{:else}
	<img
		src={shownSrc}
		alt={resolved.alt}
		class={className}
		data-fallback={shownSrc === fallbackSrc ? 'true' : 'false'}
		data-testid="track-artwork"
		onerror={handleError}
	/>
{/if}
