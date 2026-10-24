import { describe, expect, it } from 'vitest';
import {
	createQueue,
	currentTrack,
	DEMO_TRACKS,
	removeTrack,
	selectTrack,
	type QueueState
} from '#lib/player/queue.js';

/**
 * DEMOREMOVE acceptance handoff, unit half (test-owned; the paired build
 * must make these pass WITHOUT editing this file — send a wrong test back
 * to its author instead of weakening it).
 *
 * RED base: 781c4b657d3621e01c0a2f7d6e8cdc806db8ea04 (2026-10-24).
 * Observed: DM-U-03..DM-U-06 assertion-RED (demo script neither imports
 * removeTrack nor renders Remove/announcement/empty-target hooks);
 * DM-U-01/DM-U-02/DM-U-07 controls pass; no harness/import errors.
 *
 * Public contract: demo inline queue rows
 * (apps/web/src/routes/demo/player/+page.svelte) gain a per-row Remove
 * that reuses the pure `removeTrack` helper — no hand-rolled splice.
 * Removing the current track advances per removeTrack (next slides in;
 * tail falls back; sole removal empties) without restarting playback.
 * Removals announce `{title} removed from the queue` through the shared
 * `queue-announcement` polite region, and focus recovers per the queue
 * remove/clear focus semantics: the next surviving row's Remove, else
 * the previous, else the stable `queue-empty-focus` target — never the
 * body, never the now-disabled Clear button. The `queue-announcement`
 * hook is shared with the queue move-to-top/bottom slice and the
 * `queue-empty-focus` hook with the queue remove/clear focus slice;
 * all three slices agree on these hook contracts, in either merge order.
 *
 * This file pins the helper-reuse target (real removeTrack transitions
 * over the demo catalogue) plus the route's script import and markup
 * hooks via the raw-source glob idiom (same approach as
 * src/routes/login/page.test.ts). The mocked-browser companion
 * (apps/web/tests/demo-queue-remove.spec.ts) drives the real
 * /demo/player route: order/current/advance, no-restart, once-only
 * announcements, focus recovery + no-steal, touch and narrow viewport.
 * All new hooks are route literals (the e2e TESTIDS map does not cover
 * this route), following the existing queue-select-/queue-move-up-
 * literal convention.
 *
 * Reproduce from apps/web:
 *   pnpm install --frozen-lockfile
 *   pnpm test src/routes/demo/player/demoQueueRemove.acceptance.test.ts
 * Regression (must stay green before and after the build):
 *   pnpm test src/lib/player/queue.test.ts src/lib/player/playerQueue.test.ts \
 *     src/lib/player/playerQueueContracts.test.ts \
 *     src/routes/library/queue-announcement.test.ts
 * Also run pnpm check and the companion browser spec (see its header).
 * Expected on the pre-feature base: assertion RED for the missing
 * import and hooks, not an import/compile error. The Builder consumes
 * these tests in one combined ready PR; never merge the RED test bank
 * alone.
 *
 * Breaks caught: hand-rolled removal that drifts from removeTrack;
 * Remove controls that are links, divs or new tab stops; a second live
 * region instead of the shared hook; recovery with nowhere to land;
 * positive tabindex, autofocus or a dialog sneaking into the queue
 * panel; lost row identity across removal re-renders.
 */

const sources = import.meta.glob('./+page.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

function readDemoPage(): string {
	const source = sources['./+page.svelte'];
	if (typeof source !== 'string' || source.length === 0) {
		throw new Error(
			'missing demo player source: src/routes/demo/player/+page.svelte was not found by the raw glob'
		);
	}
	return source;
}

/** The queue.js import statement's braced name list. */
function queueImportNames(source: string): string {
	const match = /import\s*\{([^}]*)}\s*from\s*'#lib\/player\/queue\.js'/.exec(source);
	expect(
		match,
		'expected the demo script to import from #lib/player/queue.js (removeTrack reuse starts here)'
	).not.toBeNull();
	return match![1]!;
}

/** Opening tag of the first element carrying a hook, or null. */
function hookTag(source: string, testIdPrefix: string): string | null {
	const match = new RegExp(`<([a-zA-Z][\\w-]*)\\b[^>]*data-testid="${testIdPrefix}`).exec(
		source
	);
	if (!match) return null;
	const end = source.indexOf('>', match.index);
	return end === -1 ? null : source.slice(match.index, end + 1);
}

/** Full element for an exact hook id, allowing nested same-tag elements. */
function hookElement(source: string, testId: string): string | null {
	const opening = new RegExp(
		`<([a-zA-Z][\\w-]*)\\b[^>]*data-testid="${testId}"[^>]*/?>`
	).exec(source);
	if (!opening || opening[0].endsWith('/>')) return opening?.[0] ?? null;
	const tags = new RegExp(`</?${opening[1]}\\b[^>]*>`, 'g');
	tags.lastIndex = opening.index + opening[0].length;
	let depth = 1;
	for (let next = tags.exec(source); next; next = tags.exec(source)) {
		depth += next[0].startsWith('</') ? -1 : 1;
		if (depth === 0) return source.slice(opening.index, tags.lastIndex);
	}
	return null;
}

function textOf(markup: string): string {
	return markup
		.replace(/<[^>]*>/g, ' ')
		.replace(/\{[^}]*\}/g, ' ')
		.replace(/\s+/g, ' ')
		.trim();
}

/** Markup between the queue panel and the next section (the Builder's slice). */
function queuePanelSlice(source: string): string {
	const start = source.indexOf('data-testid="queue-panel"');
	const end = source.indexOf('data-testid="recently-played"');
	expect(start, 'expected the demo queue-panel section to survive the build').toBeGreaterThan(
		-1
	);
	expect(end, 'expected the recently-played section after the queue panel').toBeGreaterThan(
		start
	);
	return source.slice(start, end);
}

function demoQueue(currentIndex: number): QueueState {
	return selectTrack(createQueue(DEMO_TRACKS), currentIndex);
}

function ids(state: QueueState): string[] {
	return state.tracks.map((track) => track.id);
}

describe('demo per-row remove — helper-reuse contract', () => {
	it('DM-U-01 removeTrack is the reuse target: the demo advance table over the real catalogue', () => {
		// Remove middle with the first track current: order kept, current kept.
		const middle = removeTrack(demoQueue(0), 1);
		expect(ids(middle)).toEqual(['demo-track', 'third-light']);
		expect(currentTrack(middle)?.id).toBe('demo-track');
		expect(middle.currentIndex).toBe(0);

		// Remove ahead of the current track: the current track follows by id.
		const ahead = removeTrack(demoQueue(2), 0);
		expect(ids(ahead)).toEqual(['second-signal', 'third-light']);
		expect(currentTrack(ahead)?.id).toBe('third-light');
		expect(ahead.currentIndex).toBe(1);

		// Remove the current first track: the next track slides into current.
		const current = removeTrack(demoQueue(0), 0);
		expect(ids(current)).toEqual(['second-signal', 'third-light']);
		expect(currentTrack(current)?.id).toBe('second-signal');
		expect(current.currentIndex).toBe(0);

		// Remove the current tail: playback falls back to the new last track.
		const tail = removeTrack(demoQueue(2), 2);
		expect(ids(tail)).toEqual(['demo-track', 'second-signal']);
		expect(currentTrack(tail)?.id).toBe('second-signal');
		expect(tail.currentIndex).toBe(1);

		// Remove the sole track: the queue empties with no selection.
		const sole = removeTrack(createQueue([DEMO_TRACKS[0]!]), 0);
		expect(sole.tracks).toEqual([]);
		expect(sole.currentIndex).toBe(-1);
		expect(currentTrack(sole)).toBeNull();

		// Invalid indexes are identical-state no-ops, never a throw or a clear.
		for (const index of [-1, 3, 99, 1.5, Number.NaN]) {
			const queue = demoQueue(1);
			expect(removeTrack(queue, index)).toBe(queue);
		}
		expect(removeTrack(createQueue([]), 0)).toEqual({ tracks: [], currentIndex: -1 });

		// Negative: the helper never mutates its input (frozen state survives).
		const frozen = demoQueue(0);
		frozen.tracks.forEach(Object.freeze);
		Object.freeze(frozen.tracks);
		Object.freeze(frozen);
		const removed = removeTrack(frozen, 1);
		expect(ids(removed)).toEqual(['demo-track', 'third-light']);
		expect(ids(frozen)).toEqual(['demo-track', 'second-signal', 'third-light']);
	});

	it('DM-U-02 the existing demo queue hooks and keyed rows survive untouched', () => {
		const source = readDemoPage();
		for (const hook of [
			'data-testid="queue-item"',
			'data-testid="queue-select-',
			'data-testid="queue-move-up-',
			'data-testid="queue-move-down-',
			'data-testid="queue-clear"',
			'data-testid="now-playing"',
			'data-testid="player-next"',
			'data-testid="demo-audio"'
		]) {
			expect(source, `expected the existing ${hook} hook to survive`).toContain(hook);
		}
		// Stable identity: filtered rows stay keyed by track id, carrying data-track-id.
		expect(source).toContain('{#each visibleQueueEntries as { track, index } (track.id)}');
		expect(source).toContain('data-track-id={track.id}');
		// The current marker stays on the per-row select control, using the
		// full-queue index carried by each filtered entry.
		expect(source).toContain("aria-current={index === queue.currentIndex ? 'true' : undefined}");
		// Clear stays a native button, disabled exactly when the queue empties.
		expect(source).toContain('disabled={queue.tracks.length === 0}');
		// Negative: the row hook stays one <li> family in markup (script may
		// reference it for focus recovery).
		expect(source.match(/<li\b[^>]*data-testid="queue-item"/g) ?? []).toHaveLength(1);
	});

	it('DM-U-03 the demo script imports and calls removeTrack (reuse, not a re-slice)', () => {
		const source = readDemoPage();
		expect(
			queueImportNames(source),
			'demo script must import removeTrack from #lib/player/queue.js (next action: add it to the queue.js import)'
		).toMatch(/\bremoveTrack\b/);
		expect(
			source,
			'demo script must call removeTrack(queue, …) in its remove handler (next action: wire the per-row Remove through the shared helper)'
		).toContain('removeTrack(queue,');
	});

	it('DM-U-04 every demo row renders a native Remove control with a stable hook and name', () => {
		const source = readDemoPage();
		const tag = hookTag(source, 'queue-remove-');
		expect(
			tag,
			'missing per-row Remove hook data-testid="queue-remove-{track.id}" (next action: add the button to the demo queue-item rows)'
		).not.toBeNull();
		expect(tag!).toMatch(/^<button\b/);
		expect(tag!).toContain('type="button"');
		// Per-track naming in either Svelte spelling ("Remove {track.title} …"
		// or a template literal); the exact accessible name is pinned
		// behaviorally by the browser companion.
		expect(tag!).toMatch(/aria-label=/);
		expect(tag!).toMatch(/Remove/);
		expect(tag!).toContain('track.title');
		// A native row control: never a tab stop, never disabled-by-default.
		expect(tag!).not.toMatch(/tabindex=/);
		expect(tag!).not.toMatch(/\bdisabled\b/);
		expect(tag!).not.toMatch(/\bautofocus\b/);
		// Negative: no link impostor carries the Remove name.
		expect(source).not.toMatch(/<a\b[^>]*aria-label="Remove /);
	});

	it('DM-U-05 the demo queue gains the shared polite announcement hook', () => {
		const source = readDemoPage();
		const tag = hookTag(source, 'queue-announcement"');
		expect(
			tag,
			'missing data-testid="queue-announcement" on the demo route (next action: render the shared live region inside the queue panel)'
		).not.toBeNull();
		expect(tag!).toMatch(/role="status"/);
		// Negative: exactly one announcement region — no second live region.
		expect(source.match(/data-testid="queue-announcement"/g) ?? []).toHaveLength(1);
	});

	it('DM-U-06 the demo queue gains a stable programmatic empty-focus target', () => {
		const source = readDemoPage();
		const element = hookElement(source, 'queue-empty-focus');
		expect(
			element,
			'missing data-testid="queue-empty-focus" recovery target (next action: render a heading/status target for sole-removal focus)'
		).not.toBeNull();
		const tag = element!.slice(0, element!.indexOf('>') + 1);
		// A heading/status target: focusable by script, never activatable.
		expect(tag).not.toMatch(/^<(button|a|input|select|textarea)\b/i);
		expect(element!).not.toMatch(/role="button"/);
		expect(tag).toContain('tabindex="-1"');
		expect(tag).not.toMatch(/\bautofocus\b/);
		const label = textOf(element!);
		expect(label.length).toBeGreaterThan(0);
		expect(label).toMatch(/queue|empty|clear/i);
	});

	it('DM-U-07 the queue panel gains no positive tabindex, autofocus or dialog', () => {
		const panel = queuePanelSlice(readDemoPage());
		for (const match of panel.matchAll(/tabindex="([^"]*)"/g)) {
			expect(match[1], 'recovery must use tabindex -1, never a tab stop').toBe('-1');
		}
		expect(panel).not.toMatch(/\bautofocus\b/);
		expect(panel).not.toMatch(/<dialog\b/);
		expect(panel).not.toMatch(/role="dialog"/);
		expect(panel).not.toMatch(/role="alertdialog"/);
	});
});
