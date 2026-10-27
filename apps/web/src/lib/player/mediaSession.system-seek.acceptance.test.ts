import { describe, expect, it, vi } from 'vitest';
import {
	applyMediaSession,
	clearMediaSession,
	type ApplyMediaSessionOptions,
	type MediaMetadataInit,
	type MediaSessionHandlers,
	type MediaSessionLike
} from './mediaSession.js';
import { DEMO_TRACKS } from './queue.js';

/**
 * SYSTEM-SEEK acceptance handoff (tests only; protected from Builder edits).
 * Base: 9b034604bad8c489ee71effa7544132fe5017dfe
 * Public-safe handoff ref: test/system-media-seek-acceptance
 * Base evidence: unit 32 assertion failures / 1 control pass; browser 24 expect
 * failures / 1 control pass. Unchanged baselines: 1655 unit + 18 browser pass.
 * No implementation GREEN or physical-device result is claimed.
 *
 * Source of truth / execution note:
 *   pnpm --dir apps/web install --offline --frozen-lockfile
 *   pnpm --dir apps/web exec vitest run src/lib/player/mediaSession.system-seek.acceptance.test.ts --reporter=json
 *   pnpm --dir apps/web exec playwright test --config tests/system-media.playwright.config.ts player-system-seek.spec.ts --workers=1 --reporter=json
 * Existing regression controls:
 *   pnpm --dir apps/web exec vitest run src/lib/player/mediaSession --exclude src/lib/player/mediaSession.system-seek.acceptance.test.ts
 *   pnpm --dir apps/web exec playwright test --config tests/system-media.playwright.config.ts player-shortcuts.spec.ts player-lifecycle.spec.ts --workers=1
 * The test-only Playwright config owns/stops its preview at port 43819 and
 * refuses reuse (no unmanaged service, backend, staging or deploy required).
 * JSON: Vitest accepts --outputFile=<path>; Playwright accepts the environment
 * variable PLAYWRIGHT_JSON_OUTPUT_NAME=<path>. Exit 1 with AssertionError/expect
 * is expected before implementation; startup/parse errors are NOT valid RED.
 *
 * Minimal injectable seam extended on EXISTING exports, not dummy source:
 * - handlers.onSeekTo/onSeekForward/onSeekBackward receive the action details.
 * - options.positionState supplies a snapshot, or null to clear stale state.
 *   Duration must be finite and >0; publish only finite in-range position and
 *   finite positive rate. Invalid/unknown duration clears previous state.
 * - Standard setPositionState() OR setPositionState({}) clears; both accepted.
 * Structural test-local extensions compile against the pre-feature adapter.
 * RED must be ordinary assertions about missing handlers/state, never missing
 * imports/types. Do not add production stubs just to run these tests.
 *
 * Axx = adapter contracts. Pxx = position contracts. Bxx case IDs live in
 * tests/player-system-seek.spec.ts, which tests real HTMLAudioElement.currentTime
 * via stored browser action callbacks. No OS/hardware presentation is proven.
 * Mutations caught: omit/misroute one action; skip position publish/clear;
 * allow one unsupported API to abort unrelated handlers or metadata cleanup.
 * Builder: continue the exact pushed handoff SHA, keep all three new paths
 * unchanged (this unit file, player-system-seek.spec.ts and its test-only
 * system-media.playwright.config.ts), implement source + GREEN, then open ONE
 * combined ready PR. Reject negative seekTime; clamp valid nonnegative targets.
 * Only absent seekOffset defaults to 10; zero/negative/nonnumeric offsets reject.
 * No standalone intentionally-red PR. No new CLI/API: dry-run is N/A.
 */

type SeekDetails = {
	action: string;
	seekTime?: number;
	seekOffset?: number;
	fastSeek?: boolean;
};
type ActionCallback = (details: SeekDetails) => void;
type Position = { duration: number; position: number; playbackRate: number };
type PositionInput = Partial<Position>;
type SeekHandlers = MediaSessionHandlers & {
	onSeekTo: ActionCallback;
	onSeekForward: ActionCallback;
	onSeekBackward: ActionCallback;
};
type PositionOptions = ApplyMediaSessionOptions & { positionState: Position | null };

const ACTIONS = ['nexttrack', 'previoustrack', 'play', 'pause', 'seekto', 'seekforward', 'seekbackward'];
const SEEK_ACTIONS = ['seekto', 'seekforward', 'seekbackward'];

class Metadata {
	constructor(readonly init: MediaMetadataInit) {}
}

function recorder(options: { rejectAction?: string; positionApi?: 'missing' | 'throwing' } = {}) {
	const callbacks = new Map<string, ActionCallback | null>();
	const attempts: string[] = [];
	const positions: (PositionInput | null)[] = [];
	const session: MediaSessionLike & { setPositionState?: (state?: PositionInput) => void } = {
		metadata: null,
		setActionHandler(action, callback) {
			attempts.push(action);
			if (action === options.rejectAction) throw new Error('unsupported action');
			callbacks.set(action, callback);
		}
	};
	if (options.positionApi !== 'missing') {
		session.setPositionState = (state) => {
			positions.push(state && Object.keys(state).length > 0 ? { ...state } : null);
			if (options.positionApi === 'throwing') throw new Error('unsupported position state');
		};
	}
	return { session, callbacks, attempts, positions };
}

function handlers(): SeekHandlers {
	return {
		onNext: vi.fn(), onPrevious: vi.fn(), onPlay: vi.fn(), onPause: vi.fn(),
		onSeekTo: vi.fn(), onSeekForward: vi.fn(), onSeekBackward: vi.fn()
	};
}

function optionsFor(session: MediaSessionLike, positionState: Position | null = null): PositionOptions {
	return { session, positionState, MetadataCtor: Metadata, baseUrl: 'https://example.test' };
}

function fire(callbacks: Map<string, ActionCallback | null>, action: string, details = { action }) {
	const callback = callbacks.get(action);
	expect(callback, `${action} must be registered by the real adapter`).toBeTypeOf('function');
	callback?.(details);
}

describe('SYSTEM-SEEK injectable adapter', () => {
	it('MS-A01 forwards each seek action and its details to the matching consumer', () => {
		const r = recorder();
		const h = handlers();
		applyMediaSession(DEMO_TRACKS[0]!, h, optionsFor(r.session));
		const to = { action: 'seekto', seekTime: 42.25, fastSeek: true };
		const forward = { action: 'seekforward', seekOffset: 7.5 };
		const backward = { action: 'seekbackward' };
		fire(r.callbacks, 'seekto', to);
		fire(r.callbacks, 'seekforward', forward);
		fire(r.callbacks, 'seekbackward', backward);
		expect(h.onSeekTo).toHaveBeenCalledExactlyOnceWith(to);
		expect(h.onSeekForward).toHaveBeenCalledExactlyOnceWith(forward);
		expect(h.onSeekBackward).toHaveBeenCalledExactlyOnceWith(backward);
		for (const key of ['onNext', 'onPrevious', 'onPlay', 'onPause'] as const) {
			expect(h[key]).not.toHaveBeenCalled();
		}
	});

	it.each(ACTIONS)('MS-A02 isolates a browser rejecting %s from every other action', (rejected) => {
		const r = recorder({ rejectAction: rejected });
		const h = handlers();
		expect(() => applyMediaSession(DEMO_TRACKS[0]!, h, optionsFor(r.session))).not.toThrow();
		for (const action of ACTIONS.filter((name) => name !== rejected)) {
			fire(r.callbacks, action);
		}
		expect(r.session.metadata).toBeInstanceOf(Metadata);
		expect(r.attempts).toEqual(expect.arrayContaining(ACTIONS));
		// Clearing is isolated too; a rejected clear cannot strand later actions.
		expect(() => clearMediaSession(optionsFor(r.session))).not.toThrow();
		for (const action of ACTIONS.filter((name) => name !== rejected)) {
			expect(r.callbacks.get(action)).toBeNull();
		}
	});

	it('MS-A03 metadata construction failure does not strand old seek callbacks', () => {
		const r = recorder();
		const old = handlers();
		applyMediaSession(DEMO_TRACKS[0]!, old, optionsFor(r.session));
		const next = handlers();
		const options: PositionOptions = {
			...optionsFor(r.session),
			MetadataCtor: class { constructor() { throw new Error('bad artwork/metadata'); } }
		};
		expect(() => applyMediaSession(DEMO_TRACKS[1]!, next, options)).not.toThrow();
		for (const action of SEEK_ACTIONS) fire(r.callbacks, action);
		expect(next.onSeekTo).toHaveBeenCalledOnce();
		expect(next.onSeekForward).toHaveBeenCalledOnce();
		expect(next.onSeekBackward).toHaveBeenCalledOnce();
		expect(old.onSeekTo).not.toHaveBeenCalled();
		expect(r.session.metadata).toBeNull();
	});

	it('MS-A04 missing position API leaves seek and legacy transport usable', () => {
		const r = recorder({ positionApi: 'missing' });
		const h = handlers();
		expect(() => applyMediaSession(DEMO_TRACKS[0]!, h, optionsFor(r.session))).not.toThrow();
		for (const action of ACTIONS) fire(r.callbacks, action);
		for (const callback of Object.values(h)) expect(callback).toHaveBeenCalledOnce();
		expect(() => clearMediaSession(optionsFor(r.session))).not.toThrow();
	});

	it.each(['empty-queue', 'teardown'] as const)('MS-A05 %s clears every handler and published position', (mode) => {
		const r = recorder();
		const options = optionsFor(r.session, { duration: 120, position: 42, playbackRate: 1.5 });
		applyMediaSession(DEMO_TRACKS[0]!, handlers(), options);
		if (mode === 'teardown') clearMediaSession(options);
		else applyMediaSession(null, handlers(), options);
		for (const action of ACTIONS) expect(r.callbacks.get(action), action).toBeNull();
		expect(r.positions.length).toBeGreaterThan(0);
		expect(r.positions.at(-1)).toBeNull();
		expect(r.session.metadata).toBeNull();
	});

	it('MS-A06 rejecting null metadata does not prevent added handler or position cleanup', () => {
		const r = recorder();
		let metadata: unknown;
		Object.defineProperty(r.session, 'metadata', {
			get: () => metadata,
			set(value: unknown) {
				if (value === null) throw new Error('metadata clear unsupported');
				metadata = value;
			}
		});
		const options = optionsFor(r.session, { duration: 120, position: 10, playbackRate: 1 });
		applyMediaSession(DEMO_TRACKS[0]!, handlers(), options);
		expect(() => clearMediaSession(options)).not.toThrow();
		for (const action of SEEK_ACTIONS) expect(r.callbacks.get(action), action).toBeNull();
		expect(r.positions.at(-1)).toBeNull();
	});

	it.each([
		{ duration: 120, position: 0, playbackRate: 1 },
		{ duration: 120, position: 120, playbackRate: 1.5 },
		{ duration: 120, position: 42.25, playbackRate: 0.5 }
	])('MS-P01 publishes a valid finite snapshot %j', (state) => {
		const r = recorder();
		applyMediaSession(DEMO_TRACKS[0]!, handlers(), optionsFor(r.session, state));
		expect(r.positions.at(-1)).toEqual(state);
	});

	it.each([0, -1, NaN, Infinity, -Infinity])('MS-P02 unknown/invalid duration %s clears stale state', (duration) => {
		const r = recorder();
		applyMediaSession(DEMO_TRACKS[0]!, handlers(), optionsFor(r.session, { duration: 120, position: 10, playbackRate: 1 }));
		// Seed stale browser state independently, so a missing first publish
		// cannot turn the clear assertion into an accidental passing control.
		r.positions.push({ duration: 120, position: 10, playbackRate: 1 });
		applyMediaSession(DEMO_TRACKS[1]!, handlers(), optionsFor(r.session, { duration, position: 0, playbackRate: 1 }));
		expect(r.positions.at(-1)).toBeNull();
	});

	it('MS-P03 a throwing position API cannot break transport, metadata, or handler cleanup', () => {
		const r = recorder({ positionApi: 'throwing' });
		const h = handlers();
		const options = optionsFor(r.session, { duration: 120, position: 10, playbackRate: 1 });
		expect(() => applyMediaSession(DEMO_TRACKS[0]!, h, options)).not.toThrow();
		for (const action of ACTIONS) fire(r.callbacks, action);
		expect(r.session.metadata).toBeInstanceOf(Metadata);
		expect(r.positions).toContainEqual({ duration: 120, position: 10, playbackRate: 1 });
		expect(() => clearMediaSession(options)).not.toThrow();
		for (const action of ACTIONS) expect(r.callbacks.get(action)).toBeNull();
		expect(r.positions.at(-1)).toBeNull();
	});

	it.each([
		{ field: 'position', value: NaN },
		{ field: 'position', value: Infinity },
		{ field: 'position', value: -Infinity },
		{ field: 'position', value: -1 },
		{ field: 'position', value: 121 },
		{ field: 'playbackRate', value: 0 },
		{ field: 'playbackRate', value: -1 },
		{ field: 'playbackRate', value: NaN },
		{ field: 'playbackRate', value: Infinity },
		{ field: 'playbackRate', value: -Infinity }
	])('MS-P04 invalid $field=$value cannot publish illegal state or retain a prior track snapshot', ({ field, value }) => {
		const r = recorder();
		r.positions.push({ duration: 120, position: 10, playbackRate: 1 });
		const before = r.positions.length;
		const state = { duration: 120, position: 42, playbackRate: 1, [field]: value };
		applyMediaSession(DEMO_TRACKS[1]!, handlers(), optionsFor(r.session, state));
		// Clearing OR normalizing to valid numbers is allowed; silently leaving
		// the previous track's snapshot or sending illegal numbers is not.
		expect(r.positions.length).toBeGreaterThan(before);
		for (const published of r.positions.slice(before)) {
			if (published === null) continue;
			expect(Number.isFinite(published.duration) && published.duration! > 0).toBe(true);
			expect(Number.isFinite(published.position) && published.position! >= 0 && published.position! <= published.duration!).toBe(true);
			expect(Number.isFinite(published.playbackRate) && published.playbackRate! > 0).toBe(true);
		}
	});

	it('MS-A07 unsupported whole Media Session remains a silent no-op', () => {
		expect(applyMediaSession(DEMO_TRACKS[0]!, handlers(), { session: null })).toBe(false);
		expect(clearMediaSession({ session: null })).toBe(false);
	});
});
