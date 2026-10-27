import { describe, expect, it } from 'vitest';

/**
 * RED acceptance for the visible-subset selection helpers (VISIBLESELECT).
 *
 * OWNERSHIP: Test-Author owned. The paired builder adds the three helpers to
 * `apps/web/src/lib/library/selection.ts` to satisfy this file and must not
 * edit it; a wrong test comes back to the acceptance lane instead.
 *
 * RED MECHANISM: `selection.ts` already exists, but the three helpers below
 * do not. They are loaded through a variable-specifier dynamic import guarded
 * by `isVisibleSelectionModule` (a static import of a missing export would
 * fail `pnpm check` with TS2305 and fail vitest at collection — both are
 * import failures, not assertion RED). Every case gates on `requireModule`,
 * whose `not.toBeNull()` assertion names the case ID: on current code each
 * case fails as an assertion with expected (the locked contract) vs actual
 * (helpers missing). After the build the same assertions run against the real
 * helpers with no edits here.
 *
 * LOCKED CONTRACT (new exports in `apps/web/src/lib/library/selection.ts`):
 * - A selection stays an ordered `string[]` of stable track IDs in activation
 *   order: no duplicates, no empty/non-string entries. IDs compare exactly
 *   (case-sensitive), matching the keyed list and the snapshot dedupe
 *   discipline. `displayedIds` is the current canonical displayed projection
 *   (loaded search, view and ordering) in displayed order.
 * - `selectDisplayedIds(selection, displayedIds)`: appends each valid
 *   displayed ID once in displayed order, skipping IDs already present (an
 *   already-present ID keeps its position). Existing entries — hidden or
 *   displayed — keep their order. Duplicate displayed IDs append once (first
 *   occurrence); invalid displayed entries (non-string/empty) are ignored.
 *   Selection entries pass through untouched (sanitizing stays prune's job).
 *   No-op (nothing appended) returns the same reference.
 * - `deselectDisplayedIds(selection, displayedIds)`: removes every selection
 *   entry exactly matching a valid displayed ID (all occurrences, so
 *   duplicate inputs self-heal, consistent with toggle). Survivors keep their
 *   order; hidden selections are never touched. Invalid displayed entries are
 *   ignored. No-op returns the same reference.
 * - `invertDisplayedIds(selection, displayedIds)`: toggles membership for each
 *   valid displayed ID exactly once (first occurrence wins for duplicates): a
 *   present ID is removed (all occurrences), an absent ID is collected. The
 *   result is hidden survivors in original order followed by newly selected
 *   IDs in displayed order. Invalid displayed entries are ignored; invalid
 *   selection entries pass through as hidden. No-op (empty or fully invalid
 *   displayed input) returns the same reference. Inverting twice with the same
 *   displayed input restores the exact original selection.
 * - All helpers are pure: no storage, network, clock or randomness, and no
 *   `window` required. Inputs are never mutated.
 *
 * CASE INDEX (stable IDs; VISIBLESELECT-00..21 live in
 * tests/library-visible-selection.spec.ts):
 * VISIBLESELECT-30 select order, VISIBLESELECT-31 select idempotent,
 * VISIBLESELECT-32 select invalid/duplicates/case, VISIBLESELECT-33 deselect
 * survivors, VISIBLESELECT-34 deselect no-op, VISIBLESELECT-35 invert order,
 * VISIBLESELECT-36 double inversion, VISIBLESELECT-37 invert
 * invalid/duplicates/case, VISIBLESELECT-38 no input mutation,
 * VISIBLESELECT-39 no globals.
 */

interface VisibleSelectionModule {
	selectDisplayedIds: (
		selection: readonly string[],
		displayedIds: readonly string[]
	) => string[];
	deselectDisplayedIds: (
		selection: readonly string[],
		displayedIds: readonly string[]
	) => string[];
	invertDisplayedIds: (
		selection: readonly string[],
		displayedIds: readonly string[]
	) => string[];
}

function isVisibleSelectionModule(value: unknown): value is VisibleSelectionModule {
	if (value === null || typeof value !== 'object') return false;
	const candidate = value as Record<string, unknown>;
	return (
		typeof candidate['selectDisplayedIds'] === 'function' &&
		typeof candidate['deselectDisplayedIds'] === 'function' &&
		typeof candidate['invertDisplayedIds'] === 'function'
	);
}

/**
 * Deliberately typed `string` (not a literal): TypeScript resolves literal
 * dynamic imports at typecheck time. A `string` specifier defers to the
 * runtime, where vitest throws a catchable resolution error on RED.
 */
const MODULE_SPECIFIER: string = './selection.js';

async function loadModule(): Promise<VisibleSelectionModule | null> {
	try {
		const loaded: unknown = await import(MODULE_SPECIFIER);
		return isVisibleSelectionModule(loaded) ? loaded : null;
	} catch {
		return null;
	}
}

async function requireModule(caseId: string): Promise<VisibleSelectionModule> {
	const mod = await loadModule();
	expect(
		mod,
		`${caseId}: selection.ts ships selectDisplayedIds/deselectDisplayedIds/invertDisplayedIds (see this file's header)`
	).not.toBeNull();
	if (mod === null) throw new Error(`${caseId}: visible-selection helpers missing`);
	return mod;
}

describe('selectDisplayedIds', () => {
	it('VISIBLESELECT-30 appends displayed IDs once in displayed order, retaining existing order', async () => {
		const mod = await requireModule('VISIBLESELECT-30');

		expect(mod.selectDisplayedIds([], ['row-zulu', 'row-alpha'])).toEqual([
			'row-zulu',
			'row-alpha'
		]);
		// Existing activation order first, then the newly displayed IDs.
		expect(mod.selectDisplayedIds(['row-mike', 'row-bravo'], ['row-zulu', 'row-alpha'])).toEqual(
			['row-mike', 'row-bravo', 'row-zulu', 'row-alpha']
		);
		// Already-present displayed IDs keep their position; only the missing
		// one is appended.
		expect(mod.selectDisplayedIds(['row-mike', 'row-alpha'], ['row-alpha', 'row-bravo'])).toEqual(
			['row-mike', 'row-alpha', 'row-bravo']
		);
	});

	it('VISIBLESELECT-31 selecting twice is idempotent and no-ops keep the reference', async () => {
		const mod = await requireModule('VISIBLESELECT-31');

		const selection = ['row-mike', 'row-alpha'];
		expect(mod.selectDisplayedIds(selection, ['row-alpha', 'row-mike'])).toBe(selection);
		expect(mod.selectDisplayedIds(selection, [])).toBe(selection);

		const once = mod.selectDisplayedIds([], ['row-zulu', 'row-alpha']);
		expect(mod.selectDisplayedIds(once, ['row-zulu', 'row-alpha'])).toBe(once);
	});

	it('VISIBLESELECT-32 select ignores invalid and duplicate displayed IDs and matches exactly', async () => {
		const mod = await requireModule('VISIBLESELECT-32');

		expect(mod.selectDisplayedIds([], ['row-alpha', 'row-alpha', 'row-bravo'])).toEqual([
			'row-alpha',
			'row-bravo'
		]);
		const dirty = ['row-alpha', '', 42, null, undefined, true] as unknown as readonly string[];
		expect(mod.selectDisplayedIds([], dirty)).toEqual(['row-alpha']);
		expect(mod.selectDisplayedIds(['row-mike'], dirty)).toEqual(['row-mike', 'row-alpha']);
		// Case-sensitive: a differently-cased displayed ID is a distinct ID.
		expect(mod.selectDisplayedIds(['row-mike'], ['ROW-MIKE'])).toEqual(['row-mike', 'ROW-MIKE']);
		// Selection entries pass through untouched; sanitizing stays prune's job.
		expect(mod.selectDisplayedIds(['', 'row-mike'], ['row-alpha'])).toEqual([
			'',
			'row-mike',
			'row-alpha'
		]);
	});
});

describe('deselectDisplayedIds', () => {
	it('VISIBLESELECT-33 deselect removes only displayed IDs and keeps survivor order', async () => {
		const mod = await requireModule('VISIBLESELECT-33');

		expect(
			mod.deselectDisplayedIds(['row-zulu', 'row-alpha', 'row-mike', 'row-bravo'], [
				'row-alpha',
				'row-mike'
			])
		).toEqual(['row-zulu', 'row-bravo']);
		expect(mod.deselectDisplayedIds(['row-alpha', 'row-mike'], ['row-alpha', 'row-mike'])).toEqual(
			[]
		);
		// Defensive: duplicate selection inputs self-heal to zero leftovers.
		expect(mod.deselectDisplayedIds(['row-alpha', 'row-mike', 'row-alpha'], ['row-alpha'])).toEqual(
			['row-mike']
		);
	});

	it('VISIBLESELECT-34 deselect no-ops keep the reference', async () => {
		const mod = await requireModule('VISIBLESELECT-34');

		const selection = ['row-mike', 'row-alpha'];
		expect(mod.deselectDisplayedIds(selection, [])).toBe(selection);
		expect(mod.deselectDisplayedIds(selection, ['row-zulu'])).toBe(selection);
		const invalidOnly = ['', 42, null] as unknown as readonly string[];
		expect(mod.deselectDisplayedIds(selection, invalidOnly)).toBe(selection);
		expect(mod.deselectDisplayedIds([], ['row-alpha'])).toEqual([]);
	});
});

describe('invertDisplayedIds', () => {
	it('VISIBLESELECT-35 invert toggles only displayed membership, appending newly selected in displayed order', async () => {
		const mod = await requireModule('VISIBLESELECT-35');

		// Hidden survivors keep their order; the newly selected displayed ID
		// appends after them.
		expect(
			mod.invertDisplayedIds(['row-hidden', 'row-done', 'row-kept'], ['row-done', 'row-new'])
		).toEqual(['row-hidden', 'row-kept', 'row-new']);
		expect(mod.invertDisplayedIds([], ['row-alpha', 'row-bravo'])).toEqual([
			'row-alpha',
			'row-bravo'
		]);
		expect(mod.invertDisplayedIds(['row-alpha', 'row-bravo'], ['row-alpha', 'row-bravo'])).toEqual(
			[]
		);
		expect(mod.invertDisplayedIds(['row-mike'], ['row-zulu', 'row-mike'])).toEqual(['row-zulu']);
	});

	it('VISIBLESELECT-36 inverting twice with the same displayed input restores the exact selection', async () => {
		const mod = await requireModule('VISIBLESELECT-36');

		const start = ['row-bravo', 'row-delta'];
		const displayed = ['row-alpha', 'row-delta'];
		const once = mod.invertDisplayedIds(start, displayed);
		expect(once).toEqual(['row-bravo', 'row-alpha']);
		expect(mod.invertDisplayedIds(once, displayed)).toEqual(start);

		const hidden = ['row-hidden', 'row-done'];
		const wider = ['row-done', 'row-new'];
		const flipped = mod.invertDisplayedIds(hidden, wider);
		expect(flipped).toEqual(['row-hidden', 'row-new']);
		expect(mod.invertDisplayedIds(flipped, wider)).toEqual(hidden);
	});

	it('VISIBLESELECT-37 invert ignores invalid and duplicate displayed IDs and matches exactly', async () => {
		const mod = await requireModule('VISIBLESELECT-37');

		// A duplicate displayed ID toggles once, never twice.
		expect(mod.invertDisplayedIds([], ['row-alpha', 'row-alpha'])).toEqual(['row-alpha']);
		expect(mod.invertDisplayedIds(['row-alpha'], ['row-alpha', 'row-alpha'])).toEqual([]);
		const dirty = ['row-alpha', '', 42, null] as unknown as readonly string[];
		expect(mod.invertDisplayedIds([], dirty)).toEqual(['row-alpha']);
		// Case-sensitive: a differently-cased displayed ID is a distinct ID.
		expect(mod.invertDisplayedIds(['row-mike'], ['ROW-MIKE'])).toEqual(['row-mike', 'ROW-MIKE']);
		// Invalid selection entries pass through as hidden.
		expect(mod.invertDisplayedIds(['', 'row-mike'], ['row-alpha'])).toEqual([
			'',
			'row-mike',
			'row-alpha'
		]);
		const selection = ['row-mike'];
		expect(mod.invertDisplayedIds(selection, [])).toBe(selection);
	});
});

describe('visible-selection purity', () => {
	it('VISIBLESELECT-38 the helpers never mutate their inputs', async () => {
		const mod = await requireModule('VISIBLESELECT-38');

		const selection = ['row-mike', 'row-alpha'];
		const displayed = ['row-alpha', 'row-bravo'];
		const beforeSelection = JSON.stringify(selection);
		const beforeDisplayed = JSON.stringify(displayed);

		const selected = mod.selectDisplayedIds(selection, displayed);
		const deselected = mod.deselectDisplayedIds(selection, displayed);
		const inverted = mod.invertDisplayedIds(selection, displayed);

		expect(JSON.stringify(selection)).toBe(beforeSelection);
		expect(JSON.stringify(displayed)).toBe(beforeDisplayed);
		expect(selected).not.toBe(selection);
		expect(deselected).not.toBe(selection);
		expect(inverted).not.toBe(selection);
		expect(selected).toEqual(['row-mike', 'row-alpha', 'row-bravo']);
		expect(deselected).toEqual(['row-mike']);
		expect(inverted).toEqual(['row-mike', 'row-bravo']);
	});

	it('VISIBLESELECT-39 the helpers run with no window, storage or network globals', async () => {
		const mod = await requireModule('VISIBLESELECT-39');

		expect(() => mod.selectDisplayedIds([], ['row-alpha'])).not.toThrow();
		expect(() => mod.deselectDisplayedIds(['row-alpha'], ['row-alpha'])).not.toThrow();
		expect(() => mod.invertDisplayedIds(['row-alpha'], ['row-bravo'])).not.toThrow();
	});
});
