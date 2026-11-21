import type { Track } from '../player/queue.js';
import type { LibraryItem } from './cache.js';
import { displayArtist, displayTitle } from './order.js';

/** Library display metadata only; callers resolve and validate playable sources separately. */
export function queuedTrackFor(item: LibraryItem): Track {
	const album = typeof item.release === 'string' ? item.release.trim() : '';
	return {
		id: item.id,
		title: displayTitle(item),
		...(typeof item.title !== 'string' || !item.title.trim() ? { titleIsFallback: true } : {}),
		artist: displayArtist(item) ?? '',
		...(album ? { album } : {}),
		streamUrl: ''
	};
}
