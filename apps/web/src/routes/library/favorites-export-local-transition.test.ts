import { describe, expect, it } from 'vitest';

/**
 * The same-window auth transition (`notifyLocalAuthTransition`, fired by the
 * security page) cannot be driven from a browser spec: the security page and
 * the library page are never mounted together, so the library listener is
 * unreachable end to end. Its favorites-export cleanup is therefore pinned
 * against the route source, like the other raw-glob route pins in this app.
 * The cross-tab and refresh-401 paths are covered behaviorally in
 * `tests/library-favorites-export-lifecycle.spec.ts`.
 */
const sources = import.meta.glob('./+page.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

function handlerBody(): string {
	const source = Object.values(sources)[0];
	if (typeof source !== 'string' || source.length === 0) {
		throw new Error('missing library page source: ./+page.svelte was not found by the raw glob');
	}
	const fn = source.match(/function handleLocalAuthTransition\(\): void \{([\s\S]*?)\n\t\}/);
	expect(fn, 'expected a handleLocalAuthTransition() function').not.toBeNull();
	return fn![1]!;
}

describe('library same-window auth transition', () => {
	it('clears export feedback and releases the retained Blob URL before the refresher runs', () => {
		const body = handlerBody();
		const clear = body.indexOf('clearFavoritesExportFeedback();');
		const release = body.indexOf('releaseFavoritesDownload();');
		const refresher = body.indexOf('refresher.handleLocalAuthTransition();');
		expect(clear, 'export feedback must be cleared').toBeGreaterThanOrEqual(0);
		expect(release, 'the retained Blob URL must be released').toBeGreaterThanOrEqual(0);
		expect(refresher).toBeGreaterThan(clear);
		expect(refresher).toBeGreaterThan(release);
	});
});
