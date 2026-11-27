import type { Track } from '#lib/player/queue.js';
import { displayArtist, displayTitle } from './order.js';

export const LIBRARY_TAB_TITLE = 'Library — NNTune';

function titlePart(value: unknown): string {
	return typeof value === 'string'
		? value.replace(/[\p{Cc}\p{Zl}\p{Zp}]+/gu, ' ').replace(/\p{Bidi_Control}/gu, '').trim()
		: '';
}

/** Selection is not playback: the label never claims playing or buffering. */
export function libraryTabTitle(selected: Pick<Track, 'id' | 'title' | 'artist'> | null): string {
	if (!selected) return LIBRARY_TAB_TITLE;
	const title = titlePart(displayTitle(selected)) || titlePart(selected.id);
	if (!title) return LIBRARY_TAB_TITLE;
	const artist = titlePart(displayArtist(selected));
	const identity = artist ? `${title} — ${artist}` : title;
	return `Selected: ${identity} — ${LIBRARY_TAB_TITLE}`;
}
