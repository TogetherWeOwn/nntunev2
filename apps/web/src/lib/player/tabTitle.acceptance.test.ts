import { describe, expect, it } from 'vitest';
import { DEMO_TRACKS } from './queue.js';

/**
 * RED acceptance: show the now-playing demo track in the tab title (unit half).
 *
 * OWNERSHIP: Test-Author owned. The paired Builder implements
 * `apps/web/src/lib/player/tabTitle.ts` to satisfy this file and must not
 * edit it; a wrong test comes back to the acceptance card instead.
 *
 * RED MECHANISM: the module under test does not exist yet, so it is loaded
 * through a variable-specifier dynamic import (a static import would fail
 * `pnpm check` with TS2307 and fail vitest at collection — both are
 * import failures, not assertion RED). Every case gates on `requireModule`,
 * whose `not.toBeNull()` assertion names the case ID: on current code each
 * case fails as an assertion with expected (the locked contract) vs actual
 * (module missing). After the build the same assertions run against the
 * real module with no edits here.
 *
 * LOCKED CONTRACT (`apps/web/src/lib/player/tabTitle.ts`):
 * - `interface TabTitleTrack { id: string; title: string; artist: string }`.
 *   The queue `Track` is structurally assignable: the page passes demo
 *   tracks directly, no mapping (TT-U-02 proves it with real fixtures).
 * - `TAB_TITLE_PAUSED_SUFFIX = ' (paused)'` (leading space, exact).
 * - `formatNowPlayingTitle(track, playing: boolean): string`:
 *   - Display title is `track.title` verbatim, unless blank
 *     (`(title ?? '').trim() === ''`, null/undefined-safe) — then
 *     `track.id` verbatim (the ID fallback; never throws on any blank
 *     shape, including a blank id).
 *   - A blank artist (`(artist ?? '').trim() === ''`) is omitted WITH its
 *     separator (bare title); otherwise `${display} — ${artist}` with EM
 *     DASH U+2014.
 *   - Non-blank values are verbatim (surrounding spaces kept, never
 *     trimmed); unicode passes through untouched.
 *   - `playing === false` appends `TAB_TITLE_PAUSED_SUFFIX` exactly once
 *     (a literal "(paused)" inside a track title never confuses this).
 * - `resolveDemoTabTitle(originalTitle, track | null, playing): string`:
 *   a null track resolves to `originalTitle` verbatim (even `''`); any
 *   track resolves to `formatNowPlayingTitle(track, playing)`.
 * - `createDemoTabTitle(target: { title: string }): DemoTabTitle` captures
 *   `target.title` once as readonly `originalTitle`, writing nothing at
 *   creation:
 *   - `update(track | null, playing)` writes the resolved title on EVERY
 *     call (intermediate values are observable; a swap never shows stale).
 *   - `restore()` writes `originalTitle` EXACTLY ONCE per controller;
 *     further `restore()` calls write nothing, so a stale teardown after a
 *     rapid remount cannot clobber the new mount's title. `update()` stays
 *     usable after `restore()` (the controller is reusable, not disposed).
 *   - Touches ONLY `target.title`: no globals, no storage, no fetch, no
 *     transport calls. `document` is never read — the page passes it
 *     explicitly — so import and every export run with no DOM at all. This
 *     file executes in node (vitest default, no DOM globals), which IS the
 *     SSR proof: any `document`/`window` touch throws here instead.
 *
 * CASE INDEX (stable IDs; TT-B-* live in tests/demo-tab-title.spec.ts):
 * TT-U-01 export shape, TT-U-02 real-fixture format + assignability,
 * TT-U-03 paused suffix, TT-U-04 blank artist omitted, TT-U-05 ID fallback,
 * TT-U-06 blank title+artist, TT-U-07 null/undefined-safe, TT-U-08 unicode,
 * TT-U-09 verbatim spacing, TT-U-10 literal "(paused)" in title, TT-U-11
 * resolve null, TT-U-12 resolve track, TT-U-13 capture without writes,
 * TT-U-14 update writes, TT-U-15 update null, TT-U-16 restore exactly once,
 * TT-U-17 update after restore, TT-U-18 rapid swaps, TT-U-19 only title
 * touched, TT-U-20 stale teardown no-op, TT-U-21 mount/unmount cycle,
 * TT-U-22 empty original, TT-U-23 unicode original, TT-U-24 DOM-free run.
 */

interface TabTitleTrack {
	id: string;
	title: string;
	artist: string;
}

interface DemoTabTitle {
	readonly originalTitle: string;
	update(track: TabTitleTrack | null, playing: boolean): void;
	restore(): void;
}

interface TabTitleModule {
	TAB_TITLE_PAUSED_SUFFIX: string;
	formatNowPlayingTitle(track: TabTitleTrack, playing: boolean): string;
	resolveDemoTabTitle(
		originalTitle: string,
		track: TabTitleTrack | null,
		playing: boolean
	): string;
	createDemoTabTitle(target: { title: string }): DemoTabTitle;
}

/**
 * Deliberately typed `string` (not a literal): TypeScript resolves
 * literal dynamic imports at typecheck time, which would fail `pnpm check`
 * while the module does not exist yet. A `string` specifier defers to the
 * runtime, where vitest throws a catchable resolution error on RED.
 */
const MODULE_SPECIFIER: string = './tabTitle.js';

async function loadModule(): Promise<TabTitleModule | null> {
	try {
		return (await import(MODULE_SPECIFIER)) as TabTitleModule;
	} catch {
		return null;
	}
}

async function requireModule(caseId: string): Promise<TabTitleModule> {
	const mod = await loadModule();
	expect(
		mod,
		`${caseId}: tabTitle.ts ships the locked contract (see this file's header)`
	).not.toBeNull();
	if (mod === null) throw new Error(`${caseId}: tabTitle module missing`);
	return mod;
}

/** Real catalogue fixture, so assignability is proven, not assumed. */
function demoTrack(index: number, caseId: string): TabTitleTrack {
	const track = DEMO_TRACKS[index];
	expect(track, `${caseId}: demo catalogue has index ${index}`).toBeDefined();
	if (track === undefined) throw new Error(`${caseId}: demo fixture missing`);
	return track;
}

/** Fake document: counts every `title` write. */
function countingTitleTarget(initial: string): {
	target: { title: string };
	writes: string[];
	read: () => string;
} {
	const writes: string[] = [];
	let current = initial;
	return {
		target: {
			get title(): string {
				return current;
			},
			set title(value: string) {
				current = value;
				writes.push(value);
			}
		},
		writes,
		read: () => current
	};
}

describe('tab title acceptance (unit half)', () => {
	it('TT-U-01 exports the locked contract shape', async () => {
		const mod = await requireModule('TT-U-01');
		expect(mod.TAB_TITLE_PAUSED_SUFFIX, 'TT-U-01: paused suffix pin').toBe(' (paused)');
		expect(typeof mod.formatNowPlayingTitle, 'TT-U-01: format export').toBe('function');
		expect(typeof mod.resolveDemoTabTitle, 'TT-U-01: resolve export').toBe('function');
		expect(typeof mod.createDemoTabTitle, 'TT-U-01: controller export').toBe('function');
	});

	it('TT-U-02 formats real catalogue tracks with an em dash', async () => {
		const mod = await requireModule('TT-U-02');
		// No casts: passing queue Tracks proves structural assignability.
		expect(mod.formatNowPlayingTitle(demoTrack(0, 'TT-U-02'), true)).toBe(
			'Demo track — Fixture Band'
		);
		const formatted = mod.formatNowPlayingTitle(demoTrack(1, 'TT-U-02'), true);
		expect(formatted).toBe('Second Signal — Fixture Band');
		const dash = formatted.indexOf(' — ');
		expect(dash, 'TT-U-02: em-dash separator present').toBeGreaterThan(0);
		expect(
			formatted.codePointAt(dash + 1),
			'TT-U-02: separator is EM DASH U+2014, not a hyphen'
		).toBe(0x2014);
	});

	it('TT-U-03 appends the paused suffix exactly when not playing', async () => {
		const mod = await requireModule('TT-U-03');
		const track = demoTrack(2, 'TT-U-03');
		expect(mod.formatNowPlayingTitle(track, false)).toBe('Third Light — Session Players (paused)');
		expect(mod.formatNowPlayingTitle(track, true)).toBe('Third Light — Session Players');
		expect(mod.formatNowPlayingTitle(track, false).endsWith(mod.TAB_TITLE_PAUSED_SUFFIX)).toBe(
			true
		);
	});

	it('TT-U-04 omits a blank artist with its separator', async () => {
		const mod = await requireModule('TT-U-04');
		for (const artist of ['', '   ', '\t\n ']) {
			expect(mod.formatNowPlayingTitle({ id: 'x', title: 'Bare Title', artist }, true)).toBe(
				'Bare Title'
			);
			expect(mod.formatNowPlayingTitle({ id: 'x', title: 'Bare Title', artist }, false)).toBe(
				'Bare Title (paused)'
			);
		}
	});

	it('TT-U-05 falls back to the track id when the title is blank', async () => {
		const mod = await requireModule('TT-U-05');
		for (const title of ['', '   ']) {
			expect(mod.formatNowPlayingTitle({ id: 'second-signal', title, artist: 'Fixture Band' }, true)).toBe(
				'second-signal — Fixture Band'
			);
		}
	});

	it('TT-U-06 renders a bare id when title and artist are both blank', async () => {
		const mod = await requireModule('TT-U-06');
		expect(mod.formatNowPlayingTitle({ id: 'lonely-id', title: '  ', artist: '' }, true)).toBe(
			'lonely-id'
		);
		expect(mod.formatNowPlayingTitle({ id: 'lonely-id', title: '', artist: ' ' }, false)).toBe(
			'lonely-id (paused)'
		);
		// Even a blank id never throws; the output is just empty-shaped.
		expect(mod.formatNowPlayingTitle({ id: '', title: '', artist: '' }, true)).toBe('');
	});

	it('TT-U-07 treats null/undefined title and artist as missing without throwing', async () => {
		const mod = await requireModule('TT-U-07');
		const nullish = { id: 'nullish-id', title: null, artist: undefined } as unknown as TabTitleTrack;
		expect(() => mod.formatNowPlayingTitle(nullish, true)).not.toThrow();
		expect(mod.formatNowPlayingTitle(nullish, true)).toBe('nullish-id');
		const nullTitle = {
			id: 't',
			title: undefined,
			artist: 'Present Artist'
		} as unknown as TabTitleTrack;
		expect(mod.formatNowPlayingTitle(nullTitle, false)).toBe('t — Present Artist (paused)');
	});

	it('TT-U-08 passes unicode titles and artists through verbatim', async () => {
		const mod = await requireModule('TT-U-08');
		expect(
			mod.formatNowPlayingTitle({ id: 'u', title: 'Héllo 世界 🌙', artist: 'Björk 🚀' }, true)
		).toBe('Héllo 世界 🌙 — Björk 🚀');
	});

	it('TT-U-09 keeps surrounding spaces verbatim instead of trimming', async () => {
		const mod = await requireModule('TT-U-09');
		expect(
			mod.formatNowPlayingTitle({ id: 's', title: '  Padded  ', artist: ' Wide ' }, true)
		).toBe('  Padded   —  Wide ');
	});

	it('TT-U-10 never double-applies the suffix for titles containing "(paused)"', async () => {
		const mod = await requireModule('TT-U-10');
		const track: TabTitleTrack = { id: 'p', title: 'Nap (paused) Dreams', artist: 'Band' };
		expect(mod.formatNowPlayingTitle(track, true)).toBe('Nap (paused) Dreams — Band');
		expect(mod.formatNowPlayingTitle(track, false)).toBe('Nap (paused) Dreams — Band (paused)');
	});

	it('TT-U-11 resolves a null track to the original title verbatim', async () => {
		const mod = await requireModule('TT-U-11');
		expect(mod.resolveDemoTabTitle('NNTune — player resilience demo', null, true)).toBe(
			'NNTune — player resilience demo'
		);
		expect(mod.resolveDemoTabTitle('NNTune — player resilience demo', null, false)).toBe(
			'NNTune — player resilience demo'
		);
		expect(mod.resolveDemoTabTitle('', null, false)).toBe('');
	});

	it('TT-U-12 resolves a track exactly like the formatter', async () => {
		const mod = await requireModule('TT-U-12');
		const track = demoTrack(1, 'TT-U-12');
		for (const playing of [true, false]) {
			expect(mod.resolveDemoTabTitle('Original', track, playing)).toBe(
				mod.formatNowPlayingTitle(track, playing)
			);
		}
	});

	it('TT-U-13 captures the original title without writing', async () => {
		const mod = await requireModule('TT-U-13');
		const { target, writes } = countingTitleTarget('Static Page Title');
		const controller = mod.createDemoTabTitle(target);
		expect(controller.originalTitle).toBe('Static Page Title');
		expect(writes, 'TT-U-13: creation writes nothing').toEqual([]);
	});

	it('TT-U-14 update writes the resolved title for playing and paused', async () => {
		const mod = await requireModule('TT-U-14');
		const { target, writes, read } = countingTitleTarget('Original');
		const controller = mod.createDemoTabTitle(target);
		controller.update(demoTrack(0, 'TT-U-14'), true);
		expect(read()).toBe('Demo track — Fixture Band');
		controller.update(demoTrack(0, 'TT-U-14'), false);
		expect(read()).toBe('Demo track — Fixture Band (paused)');
		expect(writes).toEqual([
			'Demo track — Fixture Band',
			'Demo track — Fixture Band (paused)'
		]);
	});

	it('TT-U-15 update with null writes the original back verbatim', async () => {
		const mod = await requireModule('TT-U-15');
		const { target, read } = countingTitleTarget('Exact — Static — Title');
		const controller = mod.createDemoTabTitle(target);
		controller.update(demoTrack(1, 'TT-U-15'), true);
		expect(read()).toBe('Second Signal — Fixture Band');
		controller.update(null, false);
		expect(read()).toBe('Exact — Static — Title');
	});

	it('TT-U-16 restore writes the original exactly once', async () => {
		const mod = await requireModule('TT-U-16');
		const { target, writes, read } = countingTitleTarget('Original');
		const controller = mod.createDemoTabTitle(target);
		controller.update(demoTrack(2, 'TT-U-16'), false);
		expect(read()).toBe('Third Light — Session Players (paused)');
		controller.restore();
		controller.restore();
		controller.restore();
		expect(read()).toBe('Original');
		expect(
			writes,
			'TT-U-16: one update write plus exactly one restore write'
		).toEqual(['Third Light — Session Players (paused)', 'Original']);
	});

	it('TT-U-17 update stays usable after restore', async () => {
		const mod = await requireModule('TT-U-17');
		const { target, read } = countingTitleTarget('Original');
		const controller = mod.createDemoTabTitle(target);
		controller.update(demoTrack(0, 'TT-U-17'), true);
		controller.restore();
		expect(read()).toBe('Original');
		controller.update(demoTrack(1, 'TT-U-17'), false);
		expect(read()).toBe('Second Signal — Fixture Band (paused)');
	});

	it('TT-U-18 rapid swaps expose every intermediate title and end on the latest', async () => {
		const mod = await requireModule('TT-U-18');
		const { target, writes } = countingTitleTarget('Original');
		const controller = mod.createDemoTabTitle(target);
		controller.update(demoTrack(0, 'TT-U-18'), true);
		expect(writes.at(-1)).toBe('Demo track — Fixture Band');
		controller.update(demoTrack(1, 'TT-U-18'), true);
		expect(writes.at(-1)).toBe('Second Signal — Fixture Band');
		controller.update(demoTrack(2, 'TT-U-18'), false);
		expect(writes.at(-1)).toBe('Third Light — Session Players (paused)');
		expect(writes).toHaveLength(3);
	});

	it('TT-U-19 touches only the target title property', async () => {
		const mod = await requireModule('TT-U-19');
		let current = 'Original';
		const writes: string[] = [];
		const target = new Proxy({} as { title: string }, {
			get(_t, prop) {
				expect(String(prop), 'TT-U-19: reads only `title`').toBe('title');
				return current;
			},
			set(_t, prop, value) {
				expect(String(prop), 'TT-U-19: writes only `title`').toBe('title');
				expect(typeof value, 'TT-U-19: title writes are strings').toBe('string');
				current = value as string;
				writes.push(value as string);
				return true;
			}
		});
		const controller = mod.createDemoTabTitle(target);
		controller.update(demoTrack(0, 'TT-U-19'), true);
		controller.update(null, false);
		controller.restore();
		expect(writes.at(-1)).toBe('Original');
		expect(controller.originalTitle).toBe('Original');
	});

	it('TT-U-20 a stale teardown never clobbers a newer mount', async () => {
		const mod = await requireModule('TT-U-20');
		const { target, read } = countingTitleTarget('Original');
		const first = mod.createDemoTabTitle(target);
		first.update(demoTrack(0, 'TT-U-20'), true);
		first.restore();
		expect(read()).toBe('Original');
		// Rapid remount: the new controller captures the restored original.
		const second = mod.createDemoTabTitle(target);
		expect(second.originalTitle).toBe('Original');
		second.update(demoTrack(1, 'TT-U-20'), true);
		expect(read()).toBe('Second Signal — Fixture Band');
		// The stale first teardown already spent its single restore: no-op.
		first.restore();
		expect(read(), 'TT-U-20: stale restore must not clobber the new title').toBe(
			'Second Signal — Fixture Band'
		);
	});

	it('TT-U-21 a mount/unmount cycle writes the exact expected sequence', async () => {
		const mod = await requireModule('TT-U-21');
		const { target, writes, read } = countingTitleTarget('Original');
		const first = mod.createDemoTabTitle(target);
		first.update(demoTrack(0, 'TT-U-21'), false);
		first.restore();
		const second = mod.createDemoTabTitle(target);
		second.update(demoTrack(2, 'TT-U-21'), true);
		second.restore();
		expect(writes).toEqual([
			'Demo track — Fixture Band (paused)',
			'Original',
			'Third Light — Session Players',
			'Original'
		]);
		expect(read()).toBe('Original');
	});

	it('TT-U-22 restores an empty original title exactly', async () => {
		const mod = await requireModule('TT-U-22');
		const { target, read } = countingTitleTarget('');
		const controller = mod.createDemoTabTitle(target);
		expect(controller.originalTitle).toBe('');
		controller.update(demoTrack(0, 'TT-U-22'), true);
		expect(read()).toBe('Demo track — Fixture Band');
		controller.restore();
		expect(read()).toBe('');
	});

	it('TT-U-23 restores a unicode original title verbatim', async () => {
		const mod = await requireModule('TT-U-23');
		const { target, read } = countingTitleTarget('NNTune 🎵 プレイヤー');
		const controller = mod.createDemoTabTitle(target);
		controller.update(demoTrack(1, 'TT-U-23'), false);
		controller.restore();
		expect(read()).toBe('NNTune 🎵 プレイヤー');
	});

	it('TT-U-24 runs with no DOM globals (the SSR proof)', async () => {
		const mod = await requireModule('TT-U-24');
		expect(
			typeof document,
			'TT-U-24: this file must execute DOM-free so the contract is SSR-safe'
		).toBe('undefined');
		// The pure exports are callable here, so none of them can touch DOM.
		expect(
			mod.formatNowPlayingTitle({ id: 'x', title: 'T', artist: 'A' }, true)
		).toBe('T — A');
		expect(mod.resolveDemoTabTitle('O', null, true)).toBe('O');
	});
});
