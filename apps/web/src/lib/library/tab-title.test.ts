import { describe, expect, it } from 'vitest';
import { LIBRARY_TAB_TITLE, libraryTabTitle } from './tab-title.js';

describe('libraryTabTitle', () => {
	it('returns the exact baseline without a selection', () => {
		expect(libraryTabTitle(null)).toBe(LIBRARY_TAB_TITLE);
	});

	it('names title and artist without claiming playback', () => {
		const title = libraryTabTitle({ id: 'a', title: 'Coral Night', artist: 'Mira North' });
		expect(title).toBe('Selected: Coral Night — Mira North — Library — NNTune');
		expect(title).not.toMatch(/playing|buffering|paused/i);
	});

	it('omits a missing or blank artist and falls back to the id for a blank title', () => {
		expect(libraryTabTitle({ id: 'a', title: 'Solo', artist: ' ' })).toBe('Selected: Solo — Library — NNTune');
		expect(libraryTabTitle({ id: 'track-1', title: '', artist: '' })).toBe('Selected: track-1 — Library — NNTune');
	});

	it('never renders non-string metadata', () => {
		const malformed = { id: 'm', title: { token: 'x' }, artist: ['y'] } as unknown as Parameters<typeof libraryTabTitle>[0];
		expect(libraryTabTitle(malformed)).toBe('Selected: m — Library — NNTune');
	});
});
