/**
 * Artwork fallback: missing R2 art must never leave a broken `<img>`.
 * The view always renders a usable `src` plus non-empty `alt`, and an
 * image error swaps to the placeholder exactly once (no error loop).
 */

export const ARTWORK_PLACEHOLDER_SRC = '/placeholder-artwork.svg';

export const ARTWORK_FALLBACK_ALT = 'Track artwork (placeholder)';

export interface ResolveArtworkOptions {
	fallbackSrc?: string;
	alt?: string;
}

/** True when `src` is worth handing to an `<img>`. */
export function isUsableArtworkSrc(src: unknown): src is string {
	if (typeof src !== 'string') return false;
	const trimmed = src.trim();
	if (trimmed === '') return false;
	return (
		trimmed.startsWith('/') ||
		trimmed.startsWith('data:') ||
		trimmed.startsWith('blob:') ||
		trimmed.startsWith('http://') ||
		trimmed.startsWith('https://')
	);
}

export interface ResolvedArtwork {
	src: string;
	alt: string;
	isFallback: boolean;
}

/**
 * Resolve the artwork to render. Missing/blank/unusable sources fall
 * back to the placeholder; `alt` is never empty.
 */
export function resolveArtwork(
	src: unknown,
	options: ResolveArtworkOptions = {}
): ResolvedArtwork {
	const fallbackSrc = options.fallbackSrc ?? ARTWORK_PLACEHOLDER_SRC;
	const alt =
		typeof options.alt === 'string' && options.alt.trim() !== ''
			? options.alt
			: ARTWORK_FALLBACK_ALT;
	if (isUsableArtworkSrc(src)) {
		return { src: src.trim(), alt, isFallback: false };
	}
	return { src: fallbackSrc, alt, isFallback: true };
}

/**
 * Next `src` to use after an `<img>` error. Returns the fallback when
 * the failed source is not already the fallback, else `null` (already
 * on the placeholder: stop, do not loop, never a broken-image state).
 */
export function nextArtworkSrcAfterError(
	currentSrc: string,
	fallbackSrc: string = ARTWORK_PLACEHOLDER_SRC
): string | null {
	if (currentSrc === fallbackSrc) return null;
	return fallbackSrc;
}
