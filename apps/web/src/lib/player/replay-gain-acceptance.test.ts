import { beforeAll, describe, expect, it } from 'vitest';

// Acceptance contract: loudness leveling with ReplayGain tags.
//
// RED until the builder lands `apps/web/src/lib/player/replayGain.ts`:
// every contract test below loads that module through `requireModule`, so
// on a tree without the implementation the suite fails with assertion
// errors naming the missing module (not import crashes), while the
// fixture self-checks still pass to prove the harness is alive.
//
// Contract surface (all pure, offline, deterministic):
// - `LEVELING_STORAGE_KEY: string` — localStorage key for the toggle.
// - `parseGainDb(value: unknown): number | null` — parses ReplayGain tag
//   text such as "-7.00 dB" (bare finite numbers pass through); anything
//   else is null.
// - `gainDbToLinear(db: number): number` — amplitude conversion 10^(db/20).
// - `applyClippingGuard(linear, peak): number` — scales back to exactly
//   full scale only when peak * linear would clip; invalid peaks are
//   ignored and headroom passes through untouched.
// - `resolveTrackLinearGain(tags, { enabled, preampDb }): number` — track
//   tag wins, album tag is the fallback, untagged or disabled is exactly
//   1, preamp adds to the tag, track peak (then album peak) routes through
//   the clipping guard.
// - `applyGainToNode(node, linear): void` — drives a GainNode-like.
// - `loadLevelingEnabled(storage) / saveLevelingEnabled(storage, v)` —
//   persisted toggle; missing, corrupt, or unavailable storage reads ON.
//
// Sensitivity: converting with the power formula, ignoring tags, dropping
// the guard, or flipping the default fails the pins below — each pin names
// the production behavior it guards.
//
// Determinism: fixtures come from a fixed-seed PRNG plus hardcoded tag
// strings; no network, no clock, no randomness outside the seed.
// Rebuilding with the same seed is pinned bit-identical below.
//
// Pairing: this file is the test author's handoff — the builder implements
// against it and never edits it.

/** Minimal slice of `GainNode` the leveler drives. */
interface GainNodeLike {
	gain: { value: number };
}

/** ReplayGain tag/peak inputs as read from track metadata. */
interface LevelingTags {
	trackGainDb?: unknown;
	albumGainDb?: unknown;
	trackPeak?: unknown;
	albumPeak?: unknown;
}

/** Minimal slice of `Storage` the toggle persists through. */
interface StorageLike {
	getItem(key: string): string | null;
	setItem(key: string, value: string): void;
}

/** Shape the builder's `replayGain.ts` must expose (checked at runtime). */
interface ReplayGainModule {
	LEVELING_STORAGE_KEY: string;
	parseGainDb: (value: unknown) => number | null;
	gainDbToLinear: (db: number) => number;
	applyClippingGuard: (linear: number, peak: unknown) => number;
	resolveTrackLinearGain: (
		tags: LevelingTags,
		opts: { enabled: boolean; preampDb?: number }
	) => number;
	applyGainToNode: (node: GainNodeLike, linear: number) => void;
	loadLevelingEnabled: (storage: StorageLike | null | undefined) => boolean;
	saveLevelingEnabled: (storage: StorageLike | null | undefined, enabled: boolean) => void;
}

function isReplayGainModule(value: unknown): value is ReplayGainModule {
	if (value === null || typeof value !== 'object') return false;
	const candidate = value as Record<string, unknown>;
	return (
		typeof candidate['LEVELING_STORAGE_KEY'] === 'string' &&
		typeof candidate['parseGainDb'] === 'function' &&
		typeof candidate['gainDbToLinear'] === 'function' &&
		typeof candidate['applyClippingGuard'] === 'function' &&
		typeof candidate['resolveTrackLinearGain'] === 'function' &&
		typeof candidate['applyGainToNode'] === 'function' &&
		typeof candidate['loadLevelingEnabled'] === 'function' &&
		typeof candidate['saveLevelingEnabled'] === 'function'
	);
}

let leveling: ReplayGainModule | null = null;

beforeAll(async () => {
	try {
		// The implementation does not exist yet (RED phase); the suppression
		// below keeps `svelte-check` green. An unused `@ts-ignore` is
		// silent, so this line stays valid once the module lands.
		// @ts-ignore: replayGain.ts is the builder's deliverable, not yet present.
		const loaded: unknown = await import('./replayGain.js');
		leveling = isReplayGainModule(loaded) ? loaded : null;
	} catch {
		leveling = null;
	}
});

function requireModule(): ReplayGainModule {
	expect(leveling, 'replayGain module missing: RED until replayGain.ts lands').not.toBeNull();
	if (leveling === null) throw new Error('unreachable: leveling module missing');
	return leveling;
}

const FIXTURE_SEED = 20261022;
const REFERENCE_DB = -14;

/** Deterministic PRNG (mulberry32) so fixtures are seeded, not random. */
function mulberry32(seed: number): () => number {
	let state = seed >>> 0;
	return () => {
		state = (state + 0x6d2b79f5) >>> 0;
		let t = state;
		t = Math.imul(t ^ (t >>> 15), t | 1);
		t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
	};
}

interface LevelingFixture {
	id: string;
	/** Base loudness in dB the tag corrects toward the reference. */
	baseDb: number;
	/** ReplayGain tag text exactly as read from metadata. */
	tag: string;
	/** Track peak (linear, full scale = 1). */
	peak: number;
}

function formatTag(db: number): string {
	return `${db < 0 ? '-' : '+'}${Math.abs(db).toFixed(2)} dB`;
}

/**
 * Three fixture tracks: seeded base loudness in slots centered on
 * -17/-13/-9 dB with ±0.75 dB jitter (pairwise spread always > 2 LU),
 * tags correcting each base to the -14 dB reference, peaks in
 * [0.30, 0.50) so the clipping guard stays inert by construction.
 */
function buildFixtures(seed: number): LevelingFixture[] {
	const rand = mulberry32(seed);
	return [0, 1, 2].map((slot) => {
		const baseDb = -17 + slot * 4 + (rand() - 0.5) * 1.5;
		const tag = formatTag(REFERENCE_DB - baseDb);
		const peak = 0.3 + rand() * 0.2;
		return { id: `fixture-${slot + 1}`, baseDb, tag, peak };
	});
}

function fakeStorage(initial: Record<string, string> = {}): StorageLike {
	const map = new Map<string, string>(Object.entries(initial));
	return {
		getItem: (key: string): string | null => (map.has(key) ? (map.get(key) as string) : null),
		setItem: (key: string, value: string): void => {
			map.set(key, value);
		}
	};
}

describe('leveling fixtures', () => {
	it('are deterministic: the same seed rebuilds bit-identical fixtures', () => {
		expect(buildFixtures(FIXTURE_SEED)).toEqual(buildFixtures(FIXTURE_SEED));
	});

	it('span more than 2 LU so leveling has something to fix', () => {
		const bases = buildFixtures(FIXTURE_SEED).map((fixture) => fixture.baseDb);
		expect(Math.max(...bases) - Math.min(...bases)).toBeGreaterThan(2);
	});

	it('keep the clipping guard inert so equality pins leveling, not limiting', () => {
		for (const fixture of buildFixtures(FIXTURE_SEED)) {
			const tagDb = REFERENCE_DB - fixture.baseDb;
			expect(fixture.peak * Math.pow(10, tagDb / 20)).toBeLessThan(1);
		}
	});
});

describe('replayGain module contract', () => {
	it('exposes the leveling surface (RED until replayGain.ts lands)', () => {
		requireModule();
	});
});

describe('parseGainDb', () => {
	it('parses tag text and bare numbers', () => {
		const { parseGainDb } = requireModule();
		expect(parseGainDb('-7.00 dB')).toBe(-7);
		expect(parseGainDb('+2.50 dB')).toBe(2.5);
		expect(parseGainDb('  +0.75 DB  ')).toBe(0.75);
		expect(parseGainDb('-3.25dB')).toBe(-3.25);
		expect(parseGainDb(1.5)).toBe(1.5);
		expect(parseGainDb(-4)).toBe(-4);
	});

	it('rejects anything that is not a gain with null', () => {
		const { parseGainDb } = requireModule();
		for (const bad of [
			'loud',
			'',
			'dB',
			'--3 dB',
			'NaN',
			'Infinity',
			null,
			undefined,
			Number.NaN,
			Number.POSITIVE_INFINITY,
			Number.NEGATIVE_INFINITY,
			true,
			{},
			[]
		]) {
			expect(parseGainDb(bad), `rejected: ${String(bad)}`).toBeNull();
		}
	});
});

describe('gainDbToLinear', () => {
	it('converts amplitude dB to linear gain (10^(db/20), not the power formula)', () => {
		const { gainDbToLinear } = requireModule();
		expect(gainDbToLinear(0)).toBe(1);
		expect(gainDbToLinear(20)).toBeCloseTo(10, 10);
		expect(gainDbToLinear(-20)).toBeCloseTo(0.1, 10);
		expect(gainDbToLinear(-6.020599913279624)).toBeCloseTo(0.5, 10);
		expect(gainDbToLinear(6.020599913279624)).toBeCloseTo(2, 10);
	});
});

describe('resolveTrackLinearGain', () => {
	it('prefers the track tag and falls back to the album tag', () => {
		const { resolveTrackLinearGain } = requireModule();
		expect(
			resolveTrackLinearGain(
				{ trackGainDb: '-3.00 dB', albumGainDb: '+9.00 dB' },
				{ enabled: true }
			)
		).toBeCloseTo(Math.pow(10, -3 / 20), 10);
		expect(resolveTrackLinearGain({ albumGainDb: '+4.00 dB' }, { enabled: true })).toBeCloseTo(
			Math.pow(10, 4 / 20),
			10
		);
	});

	it('plays untagged or garbage-tagged tracks at unity gain', () => {
		const { resolveTrackLinearGain } = requireModule();
		expect(resolveTrackLinearGain({}, { enabled: true })).toBe(1);
		expect(
			resolveTrackLinearGain({ trackGainDb: 'loud', albumGainDb: null }, { enabled: true })
		).toBe(1);
		expect(resolveTrackLinearGain({ trackPeak: 0.9 }, { enabled: true })).toBe(1);
	});

	it('plays everything at unity gain while leveling is disabled', () => {
		const { resolveTrackLinearGain } = requireModule();
		for (const fixture of buildFixtures(FIXTURE_SEED)) {
			expect(
				resolveTrackLinearGain(
					{ trackGainDb: fixture.tag, trackPeak: fixture.peak },
					{ enabled: false }
				)
			).toBe(1);
		}
	});

	it('adds the preamp offset to the tag', () => {
		const { resolveTrackLinearGain } = requireModule();
		expect(
			resolveTrackLinearGain({ trackGainDb: '-3.00 dB' }, { enabled: true, preampDb: 1.5 })
		).toBeCloseTo(Math.pow(10, -1.5 / 20), 10);
	});

	it('routes peaks through the clipping guard (track peak, then album peak)', () => {
		const { resolveTrackLinearGain } = requireModule();
		expect(
			resolveTrackLinearGain(
				{ trackGainDb: '+10.00 dB', trackPeak: 0.9 },
				{ enabled: true }
			)
		).toBeCloseTo(1 / 0.9, 9);
		expect(
			resolveTrackLinearGain(
				{ albumGainDb: '+10.00 dB', albumPeak: 0.9 },
				{ enabled: true }
			)
		).toBeCloseTo(1 / 0.9, 9);
	});
});

describe('leveled loudness over fixture tracks', () => {
	function driveToNodes(enabled: boolean): { fixture: LevelingFixture; node: GainNodeLike }[] {
		const { resolveTrackLinearGain, applyGainToNode } = requireModule();
		return buildFixtures(FIXTURE_SEED).map((fixture) => {
			const node: GainNodeLike = { gain: { value: 1 } };
			const linear = resolveTrackLinearGain(
				{ trackGainDb: fixture.tag, trackPeak: fixture.peak },
				{ enabled }
			);
			applyGainToNode(node, linear);
			return { fixture, node };
		});
	}

	it('lands every fixture within ±1 LU of the reference while enabled', () => {
		const driven = driveToNodes(true);
		const outputs = driven.map(
			({ fixture, node }) => fixture.baseDb + 20 * Math.log10(node.gain.value)
		);
		for (const out of outputs) {
			expect(Math.abs(out - REFERENCE_DB)).toBeLessThanOrEqual(1);
		}
		expect(Math.max(...outputs) - Math.min(...outputs)).toBeLessThanOrEqual(1);
	});

	it('holds every gain node at unity while disabled (spread preserved)', () => {
		const driven = driveToNodes(false);
		for (const { node } of driven) {
			expect(node.gain.value).toBe(1);
		}
		const bases = driven.map(({ fixture }) => fixture.baseDb);
		expect(Math.max(...bases) - Math.min(...bases)).toBeGreaterThan(1);
	});
});

describe('applyClippingGuard', () => {
	it('scales back exactly to full scale when peak * linear would clip', () => {
		const { gainDbToLinear, applyClippingGuard } = requireModule();
		const linear = gainDbToLinear(10);
		expect(linear).toBeGreaterThan(1);
		expect(applyClippingGuard(linear, 0.9)).toBeCloseTo(1 / 0.9, 9);
	});

	it('leaves headroom passes untouched', () => {
		const { gainDbToLinear, applyClippingGuard } = requireModule();
		const linear = gainDbToLinear(-6);
		expect(applyClippingGuard(linear, 0.5)).toBe(linear);
		expect(applyClippingGuard(2, 0.5)).toBe(2);
	});

	it('ignores invalid peaks instead of muting or boosting', () => {
		const { gainDbToLinear, applyClippingGuard } = requireModule();
		const linear = gainDbToLinear(3);
		for (const peak of [0, -0.5, Number.NaN, null, undefined, 'loud']) {
			expect(applyClippingGuard(linear, peak), `peak ignored: ${String(peak)}`).toBe(linear);
		}
	});
});

describe('leveling toggle persistence', () => {
	it('pins the storage key', () => {
		expect(requireModule().LEVELING_STORAGE_KEY).toBe('nntune.leveling.enabled');
	});

	it('round-trips both toggle states', () => {
		const { loadLevelingEnabled, saveLevelingEnabled } = requireModule();
		const store = fakeStorage();
		saveLevelingEnabled(store, true);
		expect(loadLevelingEnabled(store)).toBe(true);
		saveLevelingEnabled(store, false);
		expect(loadLevelingEnabled(store)).toBe(false);
	});

	it('defaults to ON for missing, corrupt, or unavailable storage', () => {
		const { LEVELING_STORAGE_KEY, loadLevelingEnabled, saveLevelingEnabled } = requireModule();
		expect(loadLevelingEnabled(fakeStorage())).toBe(true);
		expect(loadLevelingEnabled(fakeStorage({ [LEVELING_STORAGE_KEY]: 'maybe' }))).toBe(true);
		expect(loadLevelingEnabled(null)).toBe(true);
		expect(loadLevelingEnabled(undefined)).toBe(true);
		expect(() => saveLevelingEnabled(null, true)).not.toThrow();
		expect(() => saveLevelingEnabled(undefined, false)).not.toThrow();
	});
});
