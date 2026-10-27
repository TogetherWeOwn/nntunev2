import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import PlayerQueue from './PlayerQueue.svelte';
import {
	clearQueue,
	createQueue,
	removeTrack,
	type QueueState,
	type Track
} from './queue.js';

/**
 * QUEUE REMOVE/CLEAR FOCUS acceptance handoff (test-owned; the paired build
 * must make these pass WITHOUT editing this file — send a wrong test back to
 * its author instead of weakening it).
 *
 * RED base: 6b157d35f234eca325eaf0a3c35a08c1b49ebb4a (2026-10-24).
 * Observed: QF-U02 + QF-U03 assertion-RED (empty-focus target missing);
 * QF-U01/U04-U09 controls pass; no harness/import errors.
 *
 * Public contract: keep PlayerQueue's existing inputs, queue-item hooks and
 * transport/management wiring. Removing the last surviving row (or Clear)
 * must leave a stable programmatically focusable target for keyboard
 * recovery: an element with the `queue-empty-focus` hook (a component-owned
 * literal like `queue-item`, never a route TESTIDS entry), `tabindex="-1"`,
 * and meaningful queue/empty text. It is a heading/status target, never a
 * button, link, new tab stop or modal. Per-row `Remove {title} from queue`
 * controls and the `Clear queue` button stay native buttons with no positive
 * tabindex, and remove/clear outcomes reuse the existing `queue-announcement`
 * polite region verbatim.
 *
 * The mocked-browser companion
 * (apps/web/tests/library-queue-remove-focus.spec.ts) proves the live focus
 * moves, once-only announcements and no-steal lifecycle on the real /library
 * route; this file pins the markup half with the repo's vitest SSR-render
 * pattern, driving state through the real queue store transitions.
 *
 * Reproduce from apps/web:
 *   pnpm install --frozen-lockfile
 *   pnpm test src/lib/player/playerQueue.removeFocus.test.ts
 * Regression (must stay green before and after the build):
 *   pnpm test src/lib/player/queue.test.ts src/lib/player/playerQueue.test.ts \
 *     src/lib/player/playerQueue.a11y.test.ts src/lib/player/playerQueueContracts.test.ts \
 *     src/lib/player/playerQueue.orientation.test.ts src/routes/library/queue-announcement.test.ts
 * Also run pnpm check and the companion browser spec (see its header).
 * Expected on the pre-feature base: assertion RED for the missing
 * queue-empty-focus target, not an import/compile error. The Builder consumes
 * these tests in one combined ready PR; never merge the RED test bank alone.
 *
 * Breaks caught: recovery with nowhere to land (focus falls to body);
 * Remove/Clear controls that stop being native buttons; positive tabindex or
 * autofocus sneaking into the recovery path; a new modal/dialog for empty
 * state; lost row identity across removal re-renders; announcement-pattern
 * drift that would force a second live region.
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

function renderQueue(value: QueueState, announcement = ''): string {
	return render(PlayerQueue, { props: { state: value, announcement } }).body;
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

interface NamedButton {
	label: string;
	type: string | null;
	tabindex: string | null;
	disabled: boolean;
}

function buttons(body: string): NamedButton[] {
	const found: NamedButton[] = [];
	for (const match of body.matchAll(/<button\b([^>]*)>([\s\S]*?)<\/button>/g)) {
		const attrs = match[1] ?? '';
		const inner = (match[2] ?? '')
			.replace(/<[^>]*>/g, ' ')
			.replace(/\s+/g, ' ')
			.trim();
		found.push({
			label: attrs.match(/aria-label="([^"]*)"/)?.[1] ?? inner,
			type: attrs.match(/type="([^"]*)"/)?.[1] ?? null,
			tabindex: tabindexOf(attrs),
			disabled: /\bdisabled\b/.test(attrs)
		});
	}
	return found;
}

/** Track ids carried by `queue-item` hooks, in document order. */
function queueItemIds(body: string): (string | null)[] {
	const ids: (string | null)[] = [];
	for (const tag of body.matchAll(/<[a-zA-Z][^>]*data-testid="queue-item"[^>]*>/g)) {
		ids.push(tag[0].match(/data-track-id="([^"]*)"/)?.[1] ?? null);
	}
	return ids;
}

function itemTags(body: string): string[] {
	return [...body.matchAll(/<[a-zA-Z][^>]*data-testid="queue-item"[^>]*>/g)].map((m) => m[0]);
}

/** Text content of the `now-playing` hook, or null when the hook is absent. */
function nowPlayingText(body: string): string | null {
	const match = body.match(
		/<([a-zA-Z][\w-]*)\b[^>]*data-testid="now-playing"[^>]*>([\s\S]*?)<\/\1>/
	);
	if (!match) return null;
	return text(match[2]);
}

/** Shared RED pin: the empty-queue recovery target the browser cases focus. */
function expectEmptyTarget(body: string): void {
	const target = hook(body, 'queue-empty-focus');
	expect(target, 'stable empty-queue focus target (queue-empty-focus) is missing').not.toBeNull();
	const opening = openingTag(target!);
	// A heading/status target: focusable by script, never an activatable control.
	expect(tagOf(target!)).not.toMatch(/^(button|a|input|select|textarea)$/i);
	expect(target!).not.toMatch(/role="button"/);
	expect(tabindexOf(opening)).toBe('-1');
	expect(opening).not.toMatch(/\bautofocus\b/);
	const label = text(target);
	expect(label.length).toBeGreaterThan(0);
	expect(label).toMatch(/queue|empty|clear/i);
}

describe('remove/clear focus recovery — component markup contract', () => {
	it('QF-U01 every row exposes a native Remove control naming its own track', () => {
		const body = renderQueue(state(TRACKS, 0));
		const removes = buttons(body).filter((button) => button.label.startsWith('Remove '));

		expect(removes.map((button) => button.label)).toEqual([
			'Remove Mango Season from queue',
			'Remove Apple Cart from queue',
			'Remove Zebra Crossing from queue'
		]);
		for (const remove of removes) {
			expect(remove.type).toBe('button');
			expect(remove.tabindex).toBeNull();
			expect(remove.disabled).toBe(false);
		}
		// Negative: no Remove control is a link impostor or a second live region.
		expect(body).not.toMatch(/<a\b[^>]*aria-label="Remove /);
		expect(body.match(/data-testid="queue-announcement"/g) ?? []).toHaveLength(1);
	});

	it('QF-U02 an empty queue renders a stable programmatic focus target', () => {
		expectEmptyTarget(renderQueue(createQueue([])));
	});

	it('QF-U03 removing the sole track re-renders to the empty focus target', () => {
		const after = removeTrack(state([MANGO], 0), 0);
		expect(after.tracks).toEqual([]);
		expect(after.currentIndex).toBe(-1);

		expectEmptyTarget(renderQueue(after));
	});

	it('QF-U04 Clear queue stays a native button, enabled exactly when rows survive', () => {
		for (const value of [state(TRACKS, 0), createQueue([])]) {
			const body = renderQueue(value);
			const clear = buttons(body).filter((button) => button.label === 'Clear queue');
			expect(clear).toHaveLength(1);
			expect(clear[0]!.type).toBe('button');
			expect(clear[0]!.tabindex).toBeNull();
			expect(clear[0]!.disabled).toBe(value.tracks.length === 0);
		}
	});

	it('QF-U05 neither render gains a positive tabindex, autofocus or dialog', () => {
		for (const body of [renderQueue(state(TRACKS, 1)), renderQueue(createQueue([]))]) {
			for (const match of body.matchAll(/tabindex="([^"]*)"/g)) {
				expect(match[1], 'recovery must use tabindex -1, never a tab stop').toBe('-1');
			}
			expect(body).not.toMatch(/\bautofocus\b/);
			expect(body).not.toMatch(/role="dialog"/);
			expect(body).not.toMatch(/role="alertdialog"/);
			expect(body).not.toMatch(/<dialog\b/);
		}
	});

	it('QF-U06 surviving rows keep stable unique identity across a removal re-render', () => {
		const before = renderQueue(state(TRACKS, 0));
		expect(queueItemIds(before)).toEqual(['recovery-mango', 'recovery-apple', 'recovery-zebra']);

		const after = renderQueue(removeTrack(state(TRACKS, 0), 1));
		expect(queueItemIds(after)).toEqual(['recovery-mango', 'recovery-zebra']);
		// Negative: no row lost its key and no duplicate key appeared.
		expect(queueItemIds(after)).not.toContain(null);
		expect(new Set(queueItemIds(after)).size).toBe(queueItemIds(after).length);
	});

	it('QF-U07 the existing polite region renders remove/clear outcomes verbatim', () => {
		const announced = renderQueue(state(TRACKS, 0), 'Apple Cart removed from the queue.');
		const region = hook(announced, 'queue-announcement');
		expect(region).not.toBeNull();
		expect(openingTag(region!)).toMatch(/role="status"/);
		expect(text(region)).toBe('Apple Cart removed from the queue.');

		// Negative: an empty announcement renders empty, never stale move text.
		const cleared = hook(renderQueue(createQueue([]), ''), 'queue-announcement');
		expect(text(cleared)).toBe('');
		expect(text(cleared)).not.toMatch(/moved to/i);
	});

	it('QF-U08 removal and clear keep now-playing semantics for every position', () => {
		// Removing ahead of / behind the current track keeps the current track.
		expect(nowPlayingText(renderQueue(removeTrack(state(TRACKS, 0), 2)))).toContain('Mango Season');
		expect(nowPlayingText(renderQueue(removeTrack(state(TRACKS, 2), 0)))).toContain(
			'Zebra Crossing'
		);
		// Removing the current track advances; removing the current tail falls back.
		expect(nowPlayingText(renderQueue(removeTrack(state(TRACKS, 0), 0)))).toContain('Apple Cart');
		expect(nowPlayingText(renderQueue(removeTrack(state(TRACKS, 2), 2)))).toContain(
			'Apple Cart'
		);
		// Clearing empties; the queue never repopulates or invents a selection.
		const cleared = renderQueue(clearQueue(state(TRACKS, 1)));
		expect(nowPlayingText(cleared)).toContain('Queue is empty');
		expect(queueItemIds(cleared)).toEqual([]);
		expect(nowPlayingText(cleared)).not.toMatch(/Mango|Apple|Zebra/);
	});

	it('QF-U09 the current row stays the only row-level programmatic target', () => {
		const tags = itemTags(renderQueue(state(TRACKS, 1)));
		expect(tags).toHaveLength(3);
		expect(tags.map((tag) => tabindexOf(tag))).toEqual([null, '-1', null]);
	});
});
