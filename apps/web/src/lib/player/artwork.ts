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
	if (/[\\\p{Cc}]/u.test(trimmed)) return false;
	if (trimmed.startsWith('/')) return !trimmed.startsWith('//');
	if (trimmed.startsWith('data:')) return /^data:image\/[\w.+-]+(?:;[^,]*)?,.+$/is.test(trimmed);
	if (trimmed.startsWith('blob:null/')) return trimmed.length > 'blob:null/'.length;
	const address = trimmed.startsWith('blob:') ? trimmed.slice(5) : trimmed;
	if (!/^https?:\/\/[^/?#]/.test(address)) return false;
	try {
		const url = new URL(address);
		return !url.username && !url.password && (!trimmed.startsWith('blob:') || url.pathname.length > 1);
	} catch {
		return false;
	}
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
	const alt =
		typeof options.alt === 'string' && options.alt.trim() !== ''
			? options.alt
			: ARTWORK_FALLBACK_ALT;
	if (isUsableArtworkSrc(src)) {
		return { src: src.trim(), alt, isFallback: false };
	}
	const fallback = options.fallbackSrc;
	return { src: isUsableArtworkSrc(fallback) ? fallback.trim() : ARTWORK_PLACEHOLDER_SRC, alt, isFallback: true };
}

/**
 * Next `src` to use after an `<img>` error, or null to stop retrying.
 */
export function nextArtworkSrcAfterError(
	currentSrc: string,
	fallbackSrc: string = ARTWORK_PLACEHOLDER_SRC
): string | null {
	if (!isUsableArtworkSrc(fallbackSrc)) return null;
	const fallback = fallbackSrc.trim();
	return currentSrc.trim() === fallback ? null : fallback;
}

/** Clear a previous image error when the source changes, so returning to it retries. */
export function resetFailedArtworkSrcOnChange(
	previousSrc: string,
	currentSrc: string,
	failedSrc: string | null
): string | null {
	return previousSrc === currentSrc ? failedSrc : null;
}
