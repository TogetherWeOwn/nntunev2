import { render } from 'svelte/server';
import { afterEach, expect, test, vi } from 'vitest';
import { notifyLocalAuthTransition } from '../auth-transition.js';
import TrackLikeButton from './TrackLikeButton.svelte';
import { canonicalMetadata, findNeedle, literalId } from './canonical-compare.js';
import { mountLibraryRefresher } from './mount-refresher.js';
import { LIBRARY_TAB_TITLE, libraryTabTitle } from './tab-title.js';
import type { LibraryRefresher } from './refresh.js';

afterEach(() => vi.unstubAllGlobals());

test('settling a failed refresh suspends only a mounted view', async () => {
	vi.stubGlobal('window', new EventTarget());
	const { promise, reject } = Promise.withResolvers<void>();
	const refresher = {
		state: {} as LibraryRefresher['state'],
		refresh: vi.fn().mockReturnValueOnce(Promise.reject(new Error('failed'))).mockReturnValueOnce(promise),
		handleAuthTransition: vi.fn(),
		handleLocalAuthTransition: vi.fn(),
		suspendVerification: vi.fn(),
		dispose: vi.fn()
	} satisfies LibraryRefresher;
	const stop = mountLibraryRefresher(refresher);
	await Promise.resolve();
	expect(refresher.suspendVerification).toHaveBeenCalledOnce();
	notifyLocalAuthTransition();
	stop();
	notifyLocalAuthTransition();
	expect(refresher.handleLocalAuthTransition).toHaveBeenCalledOnce();
	expect(refresher.dispose).toHaveBeenCalledOnce();

	mountLibraryRefresher(refresher)();
	reject(new Error('late'));
	await Promise.resolve();
	expect(refresher.suspendVerification).toHaveBeenCalledOnce();
});

test('comparison ignores malformed fields and keeps stable IDs literal', () => {
	const needle = findNeedle(' É ');
	expect(canonicalMetadata('E\u0301')).toBe(needle.canonical);
	expect(literalId('E\u0301')).not.toBe(needle.literal);
	expect(canonicalMetadata(['é'])).toBe('');
	expect(literalId(null)).toBe('');
});

test('tab labels remove control characters and need a usable identity', () => {
	expect(libraryTabTitle({ id: 't', title: 'Song\nTitle', artist: '\u202e' }))
		.toBe(`Selected: Song Title — ${LIBRARY_TAB_TITLE}`);
	expect(libraryTabTitle({ id: '', title: '\u0000', artist: 'Artist' })).toBe(LIBRARY_TAB_TITLE);
});

test('a Like control is unavailable until it has confirmed boolean state', () => {
	const control = { liked: undefined as unknown as boolean, pending: false, toggle: vi.fn() };
	const { body } = render(TrackLikeButton, { props: { title: 'Song', control } });
	expect(body).toContain('aria-disabled="true"');
	expect(body).toContain('aria-pressed="false"');
});
