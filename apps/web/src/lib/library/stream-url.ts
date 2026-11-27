const MEDIA_BASE = 'https://nntune.invalid';

/** Return the trimmed application-relative media path, or an empty string. */
export function resolveStreamUrl(track: { streamUrl?: unknown } | null | undefined): string {
	const raw = track?.streamUrl;
	if (typeof raw !== 'string') return '';
	const path = raw.trim();
	if (!path.startsWith('/') || path.startsWith('//') || path.includes('\\') ||
		Array.from(path).some((char) => char.charCodeAt(0) <= 0x1f)) return '';
	try {
		return new URL(path, MEDIA_BASE).origin === MEDIA_BASE ? path : '';
	} catch {
		return '';
	}
}
