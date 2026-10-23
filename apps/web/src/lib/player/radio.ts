/**
 * Auto-continue radio: a client-side seeded shuffle over the loaded
 * library. Pure helpers only; the demo player owns transport state, so the
 * ordering rules stay unit-testable (vitest runs on every host, the
 * Playwright specs cannot).
 */

import type { Track } from './queue.js';

/** Seed used when `?radio-seed=` is absent or unparseable. Fixed, so the
 * default mix is stable across reloads and neighbouring specs stay
 * deterministic. */
export const DEFAULT_RADIO_SEED = 1;

export function parseRadioSeed(raw: string | null): number {
	if (raw === null) return DEFAULT_RADIO_SEED;
	const parsed = Number.parseInt(raw, 10);
	return Number.isSafeInteger(parsed) ? parsed : DEFAULT_RADIO_SEED;
}

/** mulberry32: small, fast, deterministic across engines for a given seed. */
function seededRandom(seed: number): () => number {
	let state = seed >>> 0;
	return () => {
		state = (state + 0x6d2b79f5) >>> 0;
		let t = state;
		t = Math.imul(t ^ (t >>> 15), t | 1);
		t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
	};
}

/**
 * Seeded shuffle of the loaded library: every track exactly once (a
 * permutation), the same seed giving the same order. Never mutates its
 * input. An empty library yields an empty order (the player then ends
 * instead of starting radio).
 */
export function createRadioOrder(tracks: readonly Track[], seed: number): Track[] {
	const order = [...tracks];
	const random = seededRandom(seed);
	for (let i = order.length - 1; i > 0; i -= 1) {
		const j = Math.floor(random() * (i + 1));
		const current = order[i]!;
		order[i] = order[j]!;
		order[j] = current;
	}
	return order;
}
