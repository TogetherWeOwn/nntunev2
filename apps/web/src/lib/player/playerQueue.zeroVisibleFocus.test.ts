import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import PlayerQueue from './PlayerQueue.svelte';
import { createQueue, type QueueState, type Track } from './queue.js';

/**
 * Builder-owned zero-visible pins for remove/clear focus recovery.
 *
 * The protected QF-U bank pins the empty-queue target; this file pins the
 * sibling contract the pre-review found missing: when a filter hides every
 * surviving track, the no-matches status (not the "Queue is empty" target,
 * which would misannounce a nonempty queue) is the stable programmatically
 * focusable recovery target. Never a control, tab stop or modal.
 *
 * Reproduce from apps/web:
 *   pnpm test src/lib/player/playerQueue.zeroVisibleFocus.test.ts
 */

function track(id: string, title: string): Track {
	return {
		id,
		title,
		artist: 'Recovery Band',
		streamUrl: `/test-audio/${id}`,
		artworkUrl: null
	};
}

const MANGO = track('recovery-mango', 'Mango Season');
const APPLE = track('recovery-apple', 'Apple Cart');
const ZEBRA = track('recovery-zebra', 'Zebra Crossing');
const TRACKS = [MANGO, APPLE, ZEBRA];

function state(tracks: Track[], currentIndex: number): QueueState {
	return { ...createQueue(tracks), currentIndex };
}

function renderQueue(value: QueueState, filterQuery = ''): string {
	return render(PlayerQueue, { props: { state: value, announcement: '', filterQuery } }).body;
}

/** Extract a hook's complete element, allowing nested elements of the same tag. */
function hook(body: string, testId: string): string | null {
	const opening = new RegExp(`<([a-zA-Z][\\w-]*)\\b[^>]*data-testid="${testId}"[^>]*>`).exec(
		body
	);
	if (!opening) return null;
	const tags = new RegExp(`</?${opening[1]}\\b[^>]*>`, 'g');
	tags.lastIndex = opening.index + opening[0].length;
	let depth = 1;
	for (let next = tags.exec(body); next; next = tags.exec(body)) {
		depth += next[0].startsWith('</') ? -1 : 1;
		if (depth === 0) return body.slice(opening.index, tags.lastIndex);
	}
	return null;
}

function text(markup: string | null): string {
	return (markup ?? '')
		.replace(/<[^>]*>/g, ' ')
		.replace(/\s+/g, ' ')
		.trim();
}

function tagOf(markup: string): string {
	return markup.match(/^<([a-zA-Z][\w-]*)[\s>]/)?.[1] ?? '';
}

function openingTag(markup: string): string {
	return markup.slice(0, markup.indexOf('>') + 1);
}

function tabindexOf(opening: string): string | null {
	return opening.match(/tabindex="([^"]*)"/)?.[1] ?? null;
}

describe('remove/clear focus recovery — zero-visible fallback', () => {
	it('ZV-U01 a fully hiding filter renders the no-matches status as the recovery target', () => {
		const body = renderQueue(state(TRACKS, 0), 'zzz-no-such-track');
		const target = hook(body, 'queue-filter-empty');
		expect(target, 'zero-visible recovery target (queue-filter-empty) is missing').not.toBeNull();
		const opening = openingTag(target!);
		expect(tagOf(target!)).not.toMatch(/^(button|a|input|select|textarea)$/i);
		expect(target!).not.toMatch(/role="button"/);
		expect(tabindexOf(opening)).toBe('-1');
		expect(opening).not.toMatch(/\bautofocus\b/);
		expect(text(target)).toMatch(/no matches/i);
		// Negative: the empty-queue target must not render for a nonempty queue.
		expect(hook(body, 'queue-empty-focus')).toBeNull();
	});

	it('ZV-U02 a truly empty queue renders the empty target, never the no-matches status', () => {
		const body = renderQueue(createQueue([]), 'zzz-no-such-track');
		expect(hook(body, 'queue-empty-focus')).not.toBeNull();
		expect(hook(body, 'queue-filter-empty')).toBeNull();
	});

	it('ZV-U03 a partially matching filter renders rows and neither status target', () => {
		const body = renderQueue(state(TRACKS, 0), 'mango');
		expect(hook(body, 'queue-filter-empty')).toBeNull();
		expect(hook(body, 'queue-empty-focus')).toBeNull();
		expect(body).toContain('data-track-id="recovery-mango"');
		expect(body).not.toContain('data-track-id="recovery-apple"');
		for (const match of body.matchAll(/tabindex="([^"]*)"/g)) {
			expect(match[1], 'recovery must use tabindex -1, never a tab stop').toBe('-1');
		}
	});
});
