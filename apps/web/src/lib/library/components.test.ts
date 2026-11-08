import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import TrackRows from './TrackRows.svelte';
import FavoritesShelf from './FavoritesShelf.svelte';
import type { LibraryItem } from './cache.js';

const first: LibraryItem = { id: 'recording', occurrence_key: 'job:1', title: 'North Light', artist: 'Harbor' };
const second: LibraryItem = { ...first, occurrence_key: 'job:2' };
const noop = () => {};

describe('extracted library surfaces', () => {
	it('renders each occurrence with the unchanged selection, star, queue and menu contracts', () => {
		const { body } = render(TrackRows, { props: {
			tracks: [first, second], queuedIds: new Set(['recording']), favoriteIds: new Set(['recording']),
			selectedIds: ['recording'], openTrackMenu: noop, libraryRowClick: noop,
			interactionIds: (id) => [id], ondragitem: noop, activateSelectionRow: noop,
			selectTrack: noop, copyTrackLink: async () => {}, starLabel: () => 'Unstar North Light', toggleFavorite: noop
		} });
		expect([...body.matchAll(/data-testid="library-item"/g)]).toHaveLength(2);
		expect([...body.matchAll(/data-testid="library-queued-badge"/g)]).toHaveLength(2);
		expect([...body.matchAll(/aria-label="Select North Light"/g)]).toHaveLength(2);
		expect([...body.matchAll(/data-testid="track-more"/g)]).toHaveLength(2);
		expect(body).toContain('Unstar North Light');
		expect(body).toContain('Harbor');
	});

	it('keeps favorites export, move, clear and polite outcomes in the favorites section', () => {
		const { body } = render(FavoritesShelf, { props: {
			favoriteTracks: [first], favoritesAnnouncement: 'North Light moved to 1 of 1',
			managementFeedback: '', favoritesExportStatusShown: 'Copied favorites as text.',
			favoritesExportFallbackShown: null, confirmClear: true, openFavoriteMenu: noop,
			selectTrack: noop, moveFavoriteLabel: (_track, direction) => `Move favorite North Light ${direction}`,
			moveFavorite: async () => {}, unstarFromShelf: noop, queueFavorites: noop,
			copyFavoritesText: async () => {}, downloadFavoritesM3U: noop, requestClearFavorites: noop,
			cancelClearFavorites: noop, confirmClearFavorites: noop
		} });
		for (const hook of ['favorites-section', 'favorite-item', 'favorites-announcement',
			'favorites-export-copy', 'favorites-export-download', 'favorites-export-status']) {
			expect(body).toContain(`data-testid="${hook}"`);
		}
		expect(body).toContain('North Light moved to 1 of 1');
		expect(body).toContain('Copied favorites as text.');
		expect(body).toContain('Yes, clear local favorites');
		expect(body).toContain('aria-label="Move favorite North Light up"');
	});
});
