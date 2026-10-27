import { afterEach, describe, expect, it, vi } from 'vitest';

/**
 * RED acceptance: offer explicit recovery after prolonged playback loading
 * (unit half). Browser half: LW-B* in tests/player-long-wait.spec.ts.
 *
 * OWNERSHIP: Test-Author owned protected bank. The Builder implements
 * `apps/web/src/lib/player/longWait.ts` plus the demo-page wiring to satisfy
 * this file and must not edit it; a wrong test comes back to the acceptance
 * card instead.
 *
 * RED MECHANISM: the module under test does not exist yet, so it is loaded
 * through a variable-specifier dynamic import (a static import would fail
 * `pnpm check` with TS2307 and fail vitest at collection — both are import
 * failures, not assertion RED). Every case gates on `requireModule`, whose
 * `not.toBeNull()` assertion names the case ID: on the shipped base each
 * case fails as an assertion with expected (the locked contract) vs actual
 * (module missing). After the build the same assertions run against the
 * real module with no edits here.
 *
 * LOCKED CONTRACT (`apps/web/src/lib/player/longWait.ts`):
 * - `LONG_WAIT_MS = 15_000` exactly: the prolonged-wait threshold.
 * - `LongWaitKind = 'initial' | 'interruption'`. `initial` is a
 *   current-source first play attempt that has not reached playing;
 *   `interruption` is a wait after that same source actually played. The
 *   demo page begins `initial` when a play attempt starts loading and
 *   `interruption` when its existing buffering signal turns true; it never
 *   begins a wait from a stalled event alone.
 * - `LongWaitNotice = { sourceId: string; kind: LongWaitKind;
 *   elapsedMs: number }` (elapsed at the showing tick, >= LONG_WAIT_MS).
 * - `createLongWait({ now, onShow }): LongWait`, where `now: () => number`
 *   is an injectable MONOTONIC ms clock (the page passes a monotonic
 *   source such as `performance.now`, never a wall clock that can jump
 *   backwards) and `onShow(notice)` is called exactly once per wait, only
 *   from the tick that first shows the notice.
 * - `wait.waitingSourceId: string | null` — the source in a continuous
 *   wait, notice shown or not; null when idle.
 * - `wait.notice: LongWaitNotice | null` — the shown notice, else null.
 * - `wait.begin(sourceId, kind)`:
 *   - From idle: starts the wait at `now()`, notice stays null.
 *   - Same `sourceId` while already waiting (shown or not): no-op. It
 *     never restarts the elapsed clock, never changes the kind, and never
 *     re-shows; duplicate waiting signals cannot extend the deadline.
 *   - A different `sourceId`: replaces the wait. The old wait/notice is
 *     fenced (notice becomes null) and the new wait needs a full
 *     LONG_WAIT_MS of its own.
 *   - An empty `sourceId` throws `RangeError` and leaves state untouched:
 *     an unfenced anonymous wait can never be armed.
 * - `wait.settle(sourceId?)` cancels the wait AND any shown notice
 *   (playing, pause, end, error, replacement, unload, teardown and the
 *   notice's own Pause action all settle). With an argument it only
 *   settles when it matches the current wait — a stale source cannot
 *   cancel the current wait. Without an argument it always settles.
 *   Settling while idle is an inert no-op.
 * - `wait.tick(nowMs?): boolean` re-checks elapsed time against the
 *   CURRENT wait (`now()` when the argument is omitted). It returns true
 *   only from the one tick that first reaches `startedAt + LONG_WAIT_MS`
 *   (that tick sets `notice` and calls `onShow` once); every other tick
 *   returns false. A tick time earlier than the start (clock regression)
 *   never fires, throws, or cancels. A background-delayed tick far past
 *   the deadline still fires at most once, for the still-current source
 *   only; after settle/replacement it is inert (no resurrection).
 * - The module is pure policy: it owns no timers, DOM, storage, network
 *   or transport. The page drives `tick` from scheduler callbacks it
 *   already owns (a single deadline callback and/or its existing refresh
 *   tick), so no duplicate concurrent timers exist. This file executes
 *   in node (vitest default, no DOM globals): any DOM touch throws here.
 * - Showing the notice is elapsed-wait feedback only: it never retries,
 *   pauses, loads, advances the queue or reports failure. Those are
 *   asserted at the page level (LW-B02/B05/B06); the policy only decides
 *   when the offer appears and for which source.
 *
 * CASE INDEX (stable IDs): LW-U01 export shape + threshold, LW-U02 just
 * below 15s, LW-U03 exactly 15s, LW-U04 single late jump above,
 * LW-U05 duplicate begin does not restart, LW-U06 interruption kind
 * after settle, LW-U07 settle before deadline, LW-U08 settle after
 * shown, LW-U09 repeated wait restarts in full, LW-U10 replacement
 * fences the old source, LW-U11 stale settle cannot cancel current,
 * LW-U12 matching/no-arg settle, LW-U13 clock regression, LW-U14 shown
 * is stable (no duplicate onShow), LW-U15 empty source rejected,
 * LW-U16 no internal timers / DOM-free, LW-U17 retry cycle re-arms the
 * same source in full, LW-U18 idle tick/settle inert.
 */

type LongWaitKind = 'initial' | 'interruption';

interface LongWaitNotice {
	sourceId: string;
	kind: LongWaitKind;
	elapsedMs: number;
}

interface LongWait {
	readonly waitingSourceId: string | null;
	readonly notice: LongWaitNotice | null;
	begin(sourceId: string, kind: LongWaitKind): void;
	settle(sourceId?: string): void;
	tick(nowMs?: number): boolean;
}

interface LongWaitModule {
	LONG_WAIT_MS: number;
	createLongWait(deps: {
		now: () => number;
		onShow: (notice: LongWaitNotice) => void;
	}): LongWait;
}

/**
 * Deliberately typed `string` (not a literal): TypeScript resolves
 * literal dynamic imports at typecheck time, which would fail `pnpm check`
 * while the module does not exist yet. A `string` specifier defers to the
 * runtime, where vitest throws a catchable resolution error on RED.
 */
const MODULE_SPECIFIER: string = './longWait.js';

async function loadModule(): Promise<LongWaitModule | null> {
	try {
		return (await import(MODULE_SPECIFIER)) as LongWaitModule;
	} catch {
		return null;
	}
}

async function requireModule(caseId: string): Promise<LongWaitModule> {
	const mod = await loadModule();
	expect(
		mod,
		`${caseId}: longWait.ts ships the locked contract (see this file's header)`
	).not.toBeNull();
	if (mod === null) throw new Error(`${caseId}: longWait module missing`);
	return mod;
}

const T0 = 1_000_000;

async function harness(caseId: string) {
	const mod = await requireModule(caseId);
	const state = { now: T0 };
	const onShow = vi.fn<(notice: LongWaitNotice) => void>();
	const wait = mod.createLongWait({ now: () => state.now, onShow });
	return { mod, state, onShow, wait };
}

function shown(onShow: { mock: { calls: [LongWaitNotice][] } }): LongWaitNotice[] {
	return onShow.mock.calls.map(([notice]) => notice);
}

afterEach(() => vi.useRealTimers());

describe('prolonged playback loading acceptance (unit half)', () => {
	it('LW-U01 exports the locked contract shape and the 15s threshold', async () => {
		const { mod, wait } = await harness('LW-U01');
		expect(mod.LONG_WAIT_MS, 'LW-U01: threshold is exactly 15s').toBe(15_000);
		expect(typeof mod.createLongWait, 'LW-U01: factory export').toBe('function');
		expect(wait.waitingSourceId, 'LW-U01: starts idle').toBeNull();
		expect(wait.notice, 'LW-U01: starts without a notice').toBeNull();
	});

	it('LW-U02 just below 15s of initial loading shows nothing', async () => {
		const { state, onShow, wait } = await harness('LW-U02');
		wait.begin('track-a', 'initial');
		expect(wait.waitingSourceId).toBe('track-a');
		expect(wait.tick(T0 + 14_999), 'LW-U02: 14.999s is below the threshold').toBe(false);
		state.now = T0 + 14_999;
		expect(wait.tick(), 'LW-U02: injected clock agrees').toBe(false);
		expect(wait.notice).toBeNull();
		expect(onShow).not.toHaveBeenCalled();
	});

	it('LW-U03 exactly 15s shows once with the initial kind and elapsed', async () => {
		const { onShow, wait } = await harness('LW-U03');
		wait.begin('track-a', 'initial');
		expect(wait.tick(T0 + 15_000), 'LW-U03: the reaching tick reports the show').toBe(true);
		expect(shown(onShow)).toHaveLength(1);
		expect(shown(onShow)[0]).toMatchObject({
			sourceId: 'track-a',
			kind: 'initial',
			elapsedMs: 15_000
		});
		expect(wait.notice).toMatchObject({ sourceId: 'track-a', kind: 'initial' });
		expect(wait.waitingSourceId, 'LW-U03: the wait itself continues').toBe('track-a');
	});

	it('LW-U04 one background-delayed tick far above 15s shows at most once', async () => {
		const { onShow, wait } = await harness('LW-U04');
		wait.begin('track-a', 'initial');
		expect(wait.tick(T0 + 120_000), 'LW-U04: a delayed callback may show late').toBe(true);
		expect(shown(onShow)[0]).toMatchObject({ sourceId: 'track-a', elapsedMs: 120_000 });
		expect(wait.tick(T0 + 240_000), 'LW-U04: later ticks never re-show').toBe(false);
		expect(onShow).toHaveBeenCalledTimes(1);
	});

	it('LW-U05 duplicate begin for the same source never restarts the clock', async () => {
		const { onShow, wait } = await harness('LW-U05');
		wait.begin('track-a', 'initial');
		expect(wait.tick(T0 + 10_000)).toBe(false);
		wait.begin('track-a', 'initial');
		wait.begin('track-a', 'interruption');
		expect(
			wait.tick(T0 + 15_000),
			'LW-U05: deadline is measured from the first begin, not the duplicates'
		).toBe(true);
		expect(shown(onShow)[0]).toMatchObject({
			sourceId: 'track-a',
			kind: 'initial',
			elapsedMs: 15_000
		});
	});

	it('LW-U06 an interruption after actual playing is a fresh wait of its own kind', async () => {
		const { onShow, wait } = await harness('LW-U06');
		wait.begin('track-a', 'initial');
		wait.settle('track-a'); // the source reached playing
		wait.begin('track-a', 'interruption');
		expect(wait.tick(T0 + 14_999)).toBe(false);
		expect(wait.tick(T0 + 15_000)).toBe(true);
		expect(shown(onShow)).toHaveLength(1);
		expect(shown(onShow)[0]).toMatchObject({ sourceId: 'track-a', kind: 'interruption' });
	});

	it('LW-U07 settle before the deadline cancels the wait for good', async () => {
		const { onShow, wait } = await harness('LW-U07');
		wait.begin('track-a', 'initial');
		expect(wait.tick(T0 + 10_000)).toBe(false);
		wait.settle('track-a');
		expect(wait.waitingSourceId).toBeNull();
		expect(wait.tick(T0 + 60_000), 'LW-U07: no timer resurrection').toBe(false);
		expect(wait.notice).toBeNull();
		expect(onShow).not.toHaveBeenCalled();
	});

	it('LW-U08 settle after the notice clears it and delayed ticks stay inert', async () => {
		const { onShow, wait } = await harness('LW-U08');
		wait.begin('track-a', 'interruption');
		expect(wait.tick(T0 + 15_000)).toBe(true);
		wait.settle('track-a');
		expect(wait.notice, 'LW-U08: recovery/pause/end/error clears the notice').toBeNull();
		expect(wait.waitingSourceId).toBeNull();
		expect(wait.tick(T0 + 90_000)).toBe(false);
		expect(onShow).toHaveBeenCalledTimes(1);
	});

	it('LW-U09 a repeated wait after recovery needs a full new 15s', async () => {
		const { state, onShow, wait } = await harness('LW-U09');
		wait.begin('track-a', 'interruption');
		expect(wait.tick(T0 + 10_000)).toBe(false);
		wait.settle('track-a'); // recovered to playing at T0+10s
		state.now = T0 + 10_000;
		wait.begin('track-a', 'interruption');
		expect(
			wait.tick(T0 + 10_000 + 14_999),
			'LW-U09: the first wait does not bank elapsed time'
		).toBe(false);
		expect(wait.tick(T0 + 10_000 + 15_000)).toBe(true);
		expect(onShow).toHaveBeenCalledTimes(1);
	});

	it('LW-U10 replacing the source fences the old wait and its deadline', async () => {
		const { state, onShow, wait } = await harness('LW-U10');
		wait.begin('track-a', 'initial');
		expect(wait.tick(T0 + 10_000)).toBe(false);
		state.now = T0 + 10_000;
		wait.begin('track-b', 'initial'); // replacement at T0+10s of the old wait
		expect(wait.waitingSourceId).toBe('track-b');
		expect(wait.notice).toBeNull();
		expect(
			wait.tick(T0 + 15_000),
			'LW-U10: the old source deadline must not show for the new source'
		).toBe(false);
		expect(wait.tick(T0 + 10_000 + 15_000)).toBe(true);
		expect(shown(onShow)).toHaveLength(1);
		expect(shown(onShow)[0]).toMatchObject({ sourceId: 'track-b', kind: 'initial' });
	});

	it('LW-U11 a stale source settle cannot cancel the current wait', async () => {
		const { onShow, wait } = await harness('LW-U11');
		wait.begin('track-a', 'initial');
		wait.begin('track-b', 'initial');
		wait.settle('track-a'); // late event from the replaced source
		expect(wait.waitingSourceId, 'LW-U11: current wait survives a stale settle').toBe('track-b');
		expect(wait.tick(T0 + 15_000)).toBe(true);
		expect(shown(onShow)[0]).toMatchObject({ sourceId: 'track-b' });
	});

	it('LW-U12 a matching settle and a no-arg settle both cancel', async () => {
		const { onShow, wait } = await harness('LW-U12');
		wait.begin('track-a', 'initial');
		wait.settle('track-a');
		expect(wait.waitingSourceId).toBeNull();
		wait.begin('track-a', 'initial');
		wait.settle(); // teardown/unload: page-level unconditional cancel
		expect(wait.waitingSourceId).toBeNull();
		expect(wait.tick(T0 + 60_000)).toBe(false);
		expect(onShow).not.toHaveBeenCalled();
	});

	it('LW-U13 a clock regression never fires early, throws, or cancels', async () => {
		const { onShow, wait } = await harness('LW-U13');
		wait.begin('track-a', 'initial');
		expect(wait.tick(T0 - 500_000), 'LW-U13: tick before the start is inert').toBe(false);
		expect(wait.waitingSourceId).toBe('track-a');
		expect(wait.tick(T0 + 14_999)).toBe(false);
		expect(wait.tick(T0 + 15_000)).toBe(true);
		expect(onShow).toHaveBeenCalledTimes(1);
	});

	it('LW-U14 a shown notice is stable under duplicate signals and ticks', async () => {
		const { onShow, wait } = await harness('LW-U14');
		wait.begin('track-a', 'interruption');
		expect(wait.tick(T0 + 15_000)).toBe(true);
		wait.begin('track-a', 'interruption'); // duplicate waiting signal
		expect(wait.tick(T0 + 30_000)).toBe(false);
		expect(wait.tick(T0 + 45_000)).toBe(false);
		expect(onShow, 'LW-U14: exactly one announced transition per wait').toHaveBeenCalledTimes(1);
		expect(wait.notice).toMatchObject({
			sourceId: 'track-a',
			kind: 'interruption',
			elapsedMs: 15_000
		});
	});

	it('LW-U15 an empty source id is rejected and leaves state untouched', async () => {
		const { onShow, wait } = await harness('LW-U15');
		expect(() => wait.begin('', 'initial'), 'LW-U15: anonymous waits are forbidden').toThrow(
			RangeError
		);
		expect(wait.waitingSourceId).toBeNull();
		wait.begin('track-a', 'initial');
		expect(() => wait.begin('', 'interruption')).toThrow(RangeError);
		expect(wait.waitingSourceId, 'LW-U15: the armed wait survives the rejected begin').toBe(
			'track-a'
		);
		expect(wait.tick(T0 + 15_000)).toBe(true);
		expect(onShow).toHaveBeenCalledTimes(1);
	});

	it('LW-U16 the policy owns no timers and runs with no DOM globals', async () => {
		vi.useFakeTimers();
		const { wait } = await harness('LW-U16');
		expect(
			typeof document,
			'LW-U16: this file must execute DOM-free so the policy is page-agnostic'
		).toBe('undefined');
		wait.begin('track-a', 'initial');
		expect(wait.tick(T0 + 15_000)).toBe(true);
		wait.settle();
		expect(
			vi.getTimerCount(),
			'LW-U16: scheduling belongs to the page; the policy creates no timers'
		).toBe(0);
	});

	it('LW-U17 an explicit retry re-arms the same source for a full new wait', async () => {
		const { state, onShow, wait } = await harness('LW-U17');
		wait.begin('track-a', 'initial');
		expect(wait.tick(T0 + 15_000)).toBe(true);
		// The notice's Retry action: the page settles the shown wait and
		// begins the fresh attempt on the still-current source.
		wait.settle('track-a');
		expect(wait.notice, 'LW-U17: retry clears the shown notice immediately').toBeNull();
		state.now = T0 + 15_000;
		wait.begin('track-a', 'initial');
		expect(wait.tick(T0 + 15_000 + 14_999)).toBe(false);
		expect(wait.tick(T0 + 15_000 + 15_000)).toBe(true);
		expect(onShow).toHaveBeenCalledTimes(2);
		expect(shown(onShow)[1]).toMatchObject({ sourceId: 'track-a', kind: 'initial' });
	});

	it('LW-U18 tick and settle while idle are inert', async () => {
		const { onShow, wait } = await harness('LW-U18');
		expect(wait.tick(T0 + 999_999)).toBe(false);
		expect(() => wait.settle()).not.toThrow();
		expect(() => wait.settle('never-armed')).not.toThrow();
		expect(wait.tick()).toBe(false);
		expect(wait.waitingSourceId).toBeNull();
		expect(wait.notice).toBeNull();
		expect(onShow).not.toHaveBeenCalled();
	});
});
