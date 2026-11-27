/**
 * Search is client-rendered on purpose (`ssr = false`): it filters the
 * session-gated library API, which only exists in the browser. Same
 * discipline as the library page loader.
 */

import type { PageLoad } from './$types';

export const ssr = false;
export const _maxQueryLength = 512;
const maxRecoveryLength = 2048;
const unsafeQuery = /[\p{Cc}\p{Cs}]/u;

export function _normalizeQuery(value: string): string | null {
	if (value.length > _maxQueryLength || unsafeQuery.test(value)) return null;
	return value.trim();
}

export const load = (({ url }) => {
	const values = url.searchParams.getAll('q');
	if (values.length > 1) return { query: null, recoveryDraft: '', draftTruncated: false };
	const value = values[0] ?? '';
	const query = _normalizeQuery(value);
	// The top bar already cleared its input. Keep oversized text editable in
	// Search without applying it, and bound how much we copy into the field.
	const truncated = value.length > maxRecoveryLength;
	const prefix = truncated ? value.slice(0, maxRecoveryLength).replace(/[\uD800-\uDBFF]$/u, '') : value;
	const recoveryDraft = query === null && !unsafeQuery.test(prefix) ? prefix : '';
	return { query, recoveryDraft, draftTruncated: truncated && recoveryDraft !== '' };
}) satisfies PageLoad;
