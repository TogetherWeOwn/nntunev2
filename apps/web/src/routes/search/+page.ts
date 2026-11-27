/**
 * Search is client-rendered on purpose (`ssr = false`): it filters the
 * session-gated library API, which only exists in the browser. Same
 * discipline as the library page loader.
 */

import type { PageLoad } from './$types';

export const ssr = false;
export const _maxQueryLength = 512;

export function _normalizeQuery(value: string): string | null {
	if (value.length > _maxQueryLength || /[\p{Cc}\p{Cs}]/u.test(value)) return null;
	return value.trim();
}

export const load: PageLoad = ({ url }) => {
	const values = url.searchParams.getAll('q');
	return { query: values.length > 1 ? null : _normalizeQuery(values[0] ?? '') };
};
