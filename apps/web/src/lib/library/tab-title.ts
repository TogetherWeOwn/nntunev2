import type { Track } from '#lib/player/queue.js';
import { displayArtist, displayTitle } from './order.js';

export const LIBRARY_TAB_TITLE = 'Library — NNTune';

/** Selection is not playback: the label never claims playing or buffering. */
export function libraryTabTitle(selected: Pick<Track, 'id' | 'title' | 'artist'> | null): string {
	if (!selected) return LIBRARY_TAB_TITLE;
	const artist = displayArtist(selected);
	const identity = artist ? `${displayTitle(selected)} — ${artist}` : displayTitle(selected);
	return `Selected: ${identity} — ${LIBRARY_TAB_TITLE}`;
}
