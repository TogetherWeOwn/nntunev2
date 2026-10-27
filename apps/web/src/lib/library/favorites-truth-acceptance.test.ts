import { describe, expect, it } from 'vitest';
import * as favoritesModule from './favorites.js';
import { LIBRARY_AUTH_CONTEXT_KEY, type SnapshotStorage } from './cache.js';

/**
 * Acceptance-owned: favorites save truth and resolved/unresolved counts.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * favorites-truth implementation on top of this branch and must make these
 * tests pass WITHOUT editing this file; a wrong test comes back to the
 * acceptance lane instead of being fixed beside the implementation.
 *
 * Contract under test (mirrored by the offline
 * `library-favorites-truth` browser spec, which pins the same behavior
 * through the real library page):
 *
 * 1. The EXISTING v1 save path returns a small typed outcome and never
 *    throws. No new persistence entry point is requested:
 *    `saveFavorites(session, local, ids)` keeps its parameters, scoped key
 *    (`nntune.library.favorites.v1.<auth-context>`), JSON-array format and
 *    filtering, and gains a return value shaped exactly
 *    `{ status: 'saved' | 'unavailable' | 'write-failed' }`:
 *    - `saved`: the write was confirmed.
 *    - `unavailable`: no session/local storage, no auth context, or a
 *      denied context read. Nothing is written.
 *    - `write-failed`: the scoped `setItem` threw (quota or denied
 *      write). Nothing is written; stale persisted membership is kept.
 *    Existing callers ignore the return value, so the change is backward
 *    compatible. The outcome carries the status only: no storage
 *    exceptions, raw track ids or auth-context material.
 *    Baseline RED is an assertion on the existing export's return value
 *    (`undefined` today), not a missing import.
 *
 * 2. A new pure count projection reports how many saved favorites resolve
 *    against the loaded snapshot:
 *    `projectFavoriteCounts<T extends { id: string }>(
 *      orderedItems: readonly T[], favoriteIds: readonly string[]
 *    ): { resolved: number; unresolved: number }`
 *    - `resolved`: distinct starred ids present among the loaded rows
 *      (matched by stable id only; loaded order is irrelevant).
 *    - `unresolved`: distinct valid (non-empty string) starred ids absent
 *      from the loaded rows. Duplicates collapse; invalid entries are
 *      ignored, never counted, never thrown on.
 *    Never mutates either input, returns a fresh object per call, and has
 *    no storage, queue or network side effects.
 *    Reflect is only an assertion-first admission check for the absent
 *    export: baseline RED is an explanatory assertion, not a
 *    module/import/compiler error.
 *
 * Case IDs (stable): FAVTRUTH-U01..U08 save outcomes, FAVTRUTH-U09..U16
 * count projection, FAVTRUTH-U17..U20 existing-seam controls.
 */

type Row = { id: string; title?: string };

type ProjectFavoriteCounts = <T extends { id: string }>(
	orderedItems: readonly T[],
	favoriteIds: readonly string[]
) => { resolved: number; unresolved: number };

function projectCounts(
	ordered: readonly Row[],
	ids: readonly unknown[]
): { resolved: number; unresolved: number } {
	const candidate = Reflect.get(favoritesModule, 'projectFavoriteCounts');
	expect(
		candidate,
		'favorites truth needs the pure projectFavoriteCounts projection'
	).toBeTypeOf('function');
	return (candidate as ProjectFavoriteCounts)(ordered, ids as readonly string[]);
}

function memoryStorage(seed: Record<string, string> = {}): SnapshotStorage {
	const values = new Map(Object.entries(seed));
	return {
		getItem: (key) => values.get(key) ?? null,
		setItem: (key, value) => {
			values.set(key, value);
		},
		removeItem: (key) => {
			values.delete(key);
		}
	};
}

const BASE = 'nntune.library.favorites.v1';
const CONTEXT = 'truth-unit-account';
const KEY = `${BASE}.${CONTEXT}`;

function storages() {
	const session = memoryStorage({ [LIBRARY_AUTH_CONTEXT_KEY]: CONTEXT });
	const local = memoryStorage();
	return { session, local };
}

/** Call the existing saver without ever throwing; return its outcome for assertion. */
function savingOutcome(
	session: SnapshotStorage | null,
	local: SnapshotStorage | null,
	ids: readonly string[]
): unknown {
	let outcome: unknown = undefined;
	expect(() => {
		outcome = favoritesModule.saveFavorites(session, local, ids);
	}).not.toThrow();
	return outcome;
}

describe('favorites-truth save outcomes', () => {
	it('[FAVTRUTH-U01] a healthy write confirms saved and persists the exact ordered set', () => {
		const { session, local } = storages();
		const ids = Object.freeze(['truth-ghost-first', 'truth-unit-zulu', 'truth-ghost-last']);
		const outcome = savingOutcome(session, local, ids);
		expect(outcome).toEqual({ status: 'saved' });
		expect(Object.keys(outcome as Record<string, unknown>)).toEqual(['status']);
		expect(local.getItem(KEY)).toBe(JSON.stringify(ids));
		expect(favoritesModule.loadFavorites(session, local)).toEqual([...ids]);
		expect(ids).toEqual(['truth-ghost-first', 'truth-unit-zulu', 'truth-ghost-last']);
	});

	it('[FAVTRUTH-U02] a missing session reports unavailable and writes nothing', () => {
		const { local } = storages();
		const outcome = savingOutcome(null, local, ['truth-unit-zulu']);
		expect(outcome).toEqual({ status: 'unavailable' });
		expect(local.getItem(KEY)).toBeNull();
	});

	it('[FAVTRUTH-U03] a missing local store reports unavailable', () => {
		const { session } = storages();
		expect(savingOutcome(session, null, ['truth-unit-zulu'])).toEqual({
			status: 'unavailable'
		});
		expect(savingOutcome(null, null, ['truth-unit-zulu'])).toEqual({ status: 'unavailable' });
	});

	it('[FAVTRUTH-U04] a missing auth context reports unavailable and writes nothing', () => {
		const session = memoryStorage();
		const local = memoryStorage();
		expect(savingOutcome(session, local, ['truth-unit-zulu'])).toEqual({
			status: 'unavailable'
		});
		expect(local.getItem(KEY)).toBeNull();
		expect(local.getItem(BASE)).toBeNull();
	});

	it('[FAVTRUTH-U05] a denied context read reports unavailable without throwing', () => {
		const session: SnapshotStorage = {
			...memoryStorage({ [LIBRARY_AUTH_CONTEXT_KEY]: CONTEXT }),
			getItem: () => {
				throw new Error('synthetic privacy refusal');
			}
		};
		const { local } = storages();
		expect(savingOutcome(session, local, ['truth-unit-zulu'])).toEqual({
			status: 'unavailable'
		});
		expect(local.getItem(KEY)).toBeNull();
	});

	it('[FAVTRUTH-U06] a refused write reports write-failed and keeps stale membership', () => {
		const { session, local } = storages();
		local.setItem(KEY, JSON.stringify(['truth-unit-zulu', 'truth-ghost-first']));
		const denied: SnapshotStorage = {
			getItem: local.getItem,
			removeItem: local.removeItem,
			setItem: () => {
				throw new Error('synthetic quota refusal');
			}
		};
		const outcome = savingOutcome(
			session,
			denied,
			Object.freeze(['truth-ghost-first', 'truth-unit-zulu', 'truth-unit-alpha'])
		);
		expect(outcome).toEqual({ status: 'write-failed' });
		expect(favoritesModule.loadFavorites(session, local)).toEqual([
			'truth-unit-zulu',
			'truth-ghost-first'
		]);
	});

	it('[FAVTRUTH-U07] retrying the complete ordered set replaces stale membership and confirms saved', () => {
		const { session, local } = storages();
		local.setItem(KEY, JSON.stringify(['truth-unit-zulu']));
		const denied: SnapshotStorage = {
			getItem: local.getItem,
			removeItem: local.removeItem,
			setItem: () => {
				throw new Error('synthetic quota refusal');
			}
		};
		expect(
			savingOutcome(session, denied, ['truth-ghost-first', 'truth-unit-zulu'])
		).toEqual({ status: 'write-failed' });
		const retry = Object.freeze([
			'truth-ghost-first',
			'truth-unit-alpha',
			'truth-unit-zulu',
			'truth-ghost-last'
		]);
		expect(savingOutcome(session, local, retry)).toEqual({ status: 'saved' });
		expect(favoritesModule.loadFavorites(session, local)).toEqual([...retry]);
	});

	it('[FAVTRUTH-U08] outcomes never carry ids, contexts or exception text', () => {
		const { session, local } = storages();
		const denied: SnapshotStorage = {
			getItem: local.getItem,
			removeItem: local.removeItem,
			setItem: () => {
				throw new Error('synthetic quota refusal');
			}
		};
		const outcomes = [
			savingOutcome(session, local, ['truth-unit-zulu']),
			savingOutcome(session, denied, ['truth-unit-zulu']),
			savingOutcome(session, memoryStorage(), ['truth-unit-zulu']),
			savingOutcome(null, local, ['truth-unit-zulu'])
		];
		for (const outcome of outcomes) {
			expect(outcome, 'every save path must report a typed outcome').not.toBeNull();
			expect(typeof outcome).toBe('object');
			const serialized = JSON.stringify(outcome) ?? '';
			expect(serialized).not.toMatch(/truth-unit-zulu/);
			expect(serialized).not.toMatch(/truth-unit-account/);
			expect(serialized).not.toMatch(/quota|refusal|error/i);
		}
	});
});

describe('favorites-truth resolved/unresolved projection', () => {
	const ZULU = Object.freeze({ id: 'truth-unit-zulu', title: 'Zulu Song' });
	const ALPHA = Object.freeze({ id: 'truth-unit-alpha', title: 'Alpha Song' });
	const MIKE = Object.freeze({ id: 'truth-unit-mike', title: 'Mike Song' });

	it('[FAVTRUTH-U09] empty library and favorites resolve to zero counts', () => {
		expect(projectCounts([], [])).toEqual({ resolved: 0, unresolved: 0 });
	});

	it('[FAVTRUTH-U10] rows without favorites resolve to zero counts', () => {
		expect(projectCounts([ZULU, ALPHA], [])).toEqual({ resolved: 0, unresolved: 0 });
	});

	it('[FAVTRUTH-U11] every starred id present resolves with no unresolved remainder', () => {
		expect(projectCounts([ZULU, ALPHA, MIKE], ['truth-unit-mike', 'truth-unit-zulu'])).toEqual(
			{ resolved: 2, unresolved: 0 }
		);
	});

	it('[FAVTRUTH-U12] ghost-only ids collapse duplicates into the unresolved count', () => {
		expect(
			projectCounts([ZULU], ['truth-ghost-a', 'truth-ghost-b', 'truth-ghost-a'])
		).toEqual({ resolved: 0, unresolved: 2 });
	});

	it('[FAVTRUTH-U13] mixed states report both counts', () => {
		expect(
			projectCounts(
				[ZULU, ALPHA, MIKE],
				['truth-ghost-a', 'truth-unit-mike', 'truth-ghost-a', 'truth-unit-zulu']
			)
		).toEqual({ resolved: 2, unresolved: 1 });
	});

	it('[FAVTRUTH-U14] invalid entries are ignored, never counted, never thrown on', () => {
		const ids = Object.freeze(['truth-unit-zulu', '', 'truth-ghost-a', 42, null, undefined]);
		expect(projectCounts([ZULU, ALPHA], ids)).toEqual({ resolved: 1, unresolved: 1 });
		expect(projectCounts([], ids)).toEqual({ resolved: 0, unresolved: 2 });
	});

	it('[FAVTRUTH-U15] matching uses stable ids only; loaded order and duplicate rows are irrelevant', () => {
		const sameTitle = Object.freeze({ id: 'truth-unit-other', title: 'Zulu Song' });
		expect(projectCounts([ZULU, sameTitle], ['truth-unit-zulu'])).toEqual({
			resolved: 1,
			unresolved: 0
		});
		const counts = projectCounts([MIKE, ALPHA, ZULU], ['truth-unit-zulu', 'truth-ghost-a']);
		expect(counts).toEqual({ resolved: 1, unresolved: 1 });
		expect(projectCounts([ZULU, ZULU], ['truth-unit-zulu'])).toEqual({
			resolved: 1,
			unresolved: 0
		});
	});

	it('[FAVTRUTH-U16] projection is pure: frozen inputs untouched, fresh object per call', () => {
		const ordered = Object.freeze([ZULU, ALPHA]);
		const starred = Object.freeze(['truth-unit-zulu', 'truth-ghost-a']);
		const first = projectCounts(ordered, starred);
		const second = projectCounts(ordered, starred);
		expect(first).toEqual({ resolved: 1, unresolved: 1 });
		expect(second).toEqual(first);
		expect(second).not.toBe(first);
		expect(ordered).toEqual([ZULU, ALPHA]);
		expect(starred).toEqual(['truth-unit-zulu', 'truth-ghost-a']);
	});
});

describe('favorites-truth existing persistence controls', () => {
	it('[FAVTRUTH-U17] key, scoping and array format stay exactly as the v1 contract', () => {
		const { session, local } = storages();
		const input = Object.freeze(['truth-unit-zulu', 'truth-ghost-a', 'truth-unit-zulu', '']);
		savingOutcome(session, local, input);
		// Writes filter to non-empty strings; duplicates collapse on load, not on write.
		expect(local.getItem(KEY)).toBe('["truth-unit-zulu","truth-ghost-a","truth-unit-zulu"]');
		expect(favoritesModule.loadFavorites(session, local)).toEqual([
			'truth-unit-zulu',
			'truth-ghost-a'
		]);
		expect(input).toEqual(['truth-unit-zulu', 'truth-ghost-a', 'truth-unit-zulu', '']);
		const first = favoritesModule.loadFavorites(session, local);
		expect(first).not.toBe(input);
		first.reverse();
		expect(favoritesModule.loadFavorites(session, local)).toEqual([
			'truth-unit-zulu',
			'truth-ghost-a'
		]);
	});

	it('[FAVTRUTH-U18] corrupt or foreign stored values fall back to empty without throwing', () => {
		const { session, local } = storages();
		for (const raw of ['{not-json', '"bare-string"', '42', '{"ids":[]}', '[42,null]']) {
			local.setItem(KEY, raw);
			expect(favoritesModule.loadFavorites(session, local)).toEqual([]);
		}
		local.setItem(KEY, '[42,"truth-unit-zulu"]');
		expect(favoritesModule.loadFavorites(session, local)).toEqual(['truth-unit-zulu']);
	});

	it('[FAVTRUTH-U19] the auth boundary clears scoped and legacy keys and never throws', () => {
		const session = memoryStorage({ [LIBRARY_AUTH_CONTEXT_KEY]: CONTEXT });
		const local = memoryStorage({
			[KEY]: '["truth-unit-zulu"]',
			[BASE]: '["legacy-id"]',
			[`${BASE}.other-account`]: '["other-private-id"]',
			'unrelated.preference': 'unchanged'
		});
		expect(() => favoritesModule.clearFavoritesAtAuthBoundary(session, local)).not.toThrow();
		expect(local.getItem(KEY)).toBeNull();
		expect(local.getItem(BASE)).toBeNull();
		expect(local.getItem(`${BASE}.other-account`)).toBe('["other-private-id"]');
		expect(local.getItem('unrelated.preference')).toBe('unchanged');
		const failing: SnapshotStorage = {
			getItem: () => {
				throw new Error('synthetic refusal');
			},
			setItem: () => {
				throw new Error('synthetic refusal');
			},
			removeItem: () => {
				throw new Error('synthetic refusal');
			}
		};
		expect(() => favoritesModule.clearFavoritesAtAuthBoundary(failing, failing)).not.toThrow();
		expect(() => favoritesModule.clearFavoritesAtAuthBoundary(null, null)).not.toThrow();
		expect(favoritesModule.favoritesKey(null)).toBeNull();
		expect(favoritesModule.favoritesKey(memoryStorage())).toBeNull();
	});

	it('[FAVTRUTH-U20] denied reads fall back to empty without throwing', () => {
		const { session } = storages();
		const denied: SnapshotStorage = {
			...memoryStorage({ [KEY]: '["truth-unit-zulu"]' }),
			getItem: () => {
				throw new Error('synthetic privacy refusal');
			}
		};
		expect(favoritesModule.loadFavorites(session, denied)).toEqual([]);
		expect(favoritesModule.loadFavorites(null, denied)).toEqual([]);
	});
});
