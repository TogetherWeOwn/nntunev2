import { describe, expect, it } from 'vitest';

/**
 * RED acceptance for the contiguous range-selection primitive.
 *
 * OWNERSHIP: Test-Author owned. The paired builder implements `selectRange`
 * in `apps/web/src/lib/library/selection.ts` to satisfy this file and must
 * not edit it; a wrong test comes back to the acceptance lane instead.
 *
 * RED MECHANISM: `selection.ts` exists but exports toggle-only helpers, so
 * the module is loaded through a variable-specifier dynamic import (a static
 * import of the missing export would fail `pnpm check` with TS2305 and fail
 * vitest at collection — both are import failures, not assertion RED). Every
 * case gates on `requireModule`, whose `not.toBeNull()` assertion names the
 * case ID: on current code each case fails as an assertion with expected
 * (the locked contract) vs actual (export missing). After the build the same
 * assertions run against the real export with no edits here.
 *
 * LOCKED CONTRACT (`selectRange` in `apps/web/src/lib/library/selection.ts`):
 * - Signature: `selectRange(selection, displayedIds, anchorId, focusId)`:
 *   `selection` is the current ordered `string[]` in activation order;
 *   `displayedIds` is the visible row IDs in displayed order (post find,
 *   order and view projections); `anchorId` is the most recently activated
 *   checkbox ID or `null` when no anchor survives (after Clear selection,
 *   the auth/account purge, or a successful-snapshot prune that dropped the
 *   anchor); `focusId` is the Shift+click / Shift+Space target row ID.
 * - The primitive is purely additive and display-scoped: with a usable
 *   anchor it selects the contiguous run from the anchor through the focus
 *   in displayed order. IDs compare exactly (case-sensitive), matching the
 *   existing selection discipline.
 * - Anchor usable means: a non-empty string present in `displayedIds`
 *   (first occurrence wins when `displayedIds` repeats an ID; non-string
 *   and empty entries in `displayedIds` are ignored for span computation).
 *   Focus usable means: a non-empty string present in `displayedIds`.
 * - Usable anchor + usable focus: the result keeps every existing selection
 *   entry untouched and in place (including any invalid entries — sanitizing
 *   stays the prune path's job), then appends each run ID not already
 *   selected in displayed-index-ascending order, regardless of range
 *   direction. Anchor === focus selects that single row (a no-op returning
 *   the same reference when it is already selected — never a toggle-off).
 *   A run that adds nothing returns the same reference.
 * - Missing anchor (null, invalid, or absent from `displayedIds`) with a
 *   usable focus: plain-toggle fallback — the result is identical in value
 *   to `toggleSelected(selection, focusId)`. This is the honest reset: the
 *   next Shift+click after clear/purge/anchor-prune (or with the anchor
 *   hidden by find/view) toggles exactly one ID.
 * - Unusable focus (invalid, or absent from `displayedIds`, including an
 *   empty `displayedIds`): no-op returning the same reference, even with a
 *   usable anchor. Hidden rows are never touched.
 * - All helpers are pure: no storage, network, clock or randomness, and no
 *   `window` required. Inputs are never mutated.
 *
 * CASE INDEX (stable IDs; RNG-B-* live in tests/library-range-select.spec.ts):
 * RNG-U-01 forward range, RNG-U-02 backward range appends displayed-ascending,
 * RNG-U-03 anchor==focus selected is a no-op, RNG-U-04 anchor==focus
 * unselected adds one, RNG-U-05 null anchor toggles on, RNG-U-06 null anchor
 * toggles off, RNG-U-07 invalid anchor toggles, RNG-U-08 undisplayed anchor
 * toggles, RNG-U-09 undisplayed focus is a no-op, RNG-U-10 invalid focus is a
 * no-op, RNG-U-11 invalid displayed entries ignored, RNG-U-12 duplicate
 * displayed IDs first-wins, RNG-U-13 middles keep activation order,
 * RNG-U-14 case-sensitive IDs, RNG-U-15 no input mutation, RNG-U-16 invalid
 * selection entries pass through, RNG-U-17 empty displayed is a no-op,
 * RNG-U-18 no globals.
 */

interface RangeSelectionModule {
	selectRange: (
		selection: readonly string[],
		displayedIds: readonly string[],
		anchorId: string | null,
		focusId: unknown
	) => string[];
}

function isRangeSelectionModule(value: unknown): value is RangeSelectionModule {
	if (value === null || typeof value !== 'object') return false;
	const candidate = value as Record<string, unknown>;
	return typeof candidate['selectRange'] === 'function';
}

/**
 * Deliberately typed `string` (not a literal): TypeScript resolves literal
 * dynamic imports at typecheck time. A `string` specifier defers to the
 * runtime, where the missing export resolves to a gateable null on RED.
 */
const MODULE_SPECIFIER: string = './selection.js';

async function loadModule(): Promise<RangeSelectionModule | null> {
	try {
		const loaded: unknown = await import(MODULE_SPECIFIER);
		return isRangeSelectionModule(loaded) ? loaded : null;
	} catch {
		return null;
	}
}

async function requireModule(caseId: string): Promise<RangeSelectionModule> {
	const mod = await loadModule();
	expect(
		mod,
		`${caseId}: selection.ts ships selectRange under the locked contract (see this file's header)`
	).not.toBeNull();
	if (mod === null) throw new Error(`${caseId}: selectRange export missing`);
	return mod;
}

/** Displayed order mirrors the browser bank's API-order fixture. */
const DISPLAYED: readonly string[] = [
	'row-zulu',
	'row-alpha',
	'row-mike',
	'row-bravo',
	'row-kilo',
	'row-delta'
];

describe('range fixtures (harness self-checks, no module required)', () => {
	it('fixture IDs are unique and ordered for forward/backward spans', () => {
		expect(new Set(DISPLAYED).size).toBe(DISPLAYED.length);
		expect(DISPLAYED.indexOf('row-alpha')).toBeLessThan(DISPLAYED.indexOf('row-kilo'));
	});
});

describe('contiguous range application', () => {
	it('RNG-U-01 a forward range appends the displayed slice in order', async () => {
		const mod = await requireModule('RNG-U-01');

		expect(mod.selectRange(['row-alpha'], DISPLAYED, 'row-alpha', 'row-kilo')).toEqual([
			'row-alpha',
			'row-mike',
			'row-bravo',
			'row-kilo'
		]);
	});

	it('RNG-U-02 a backward range appends displayed-ascending, not anchor-first', async () => {
		const mod = await requireModule('RNG-U-02');

		// Anchor at index 4, focus at index 1: the run is indices 1..4 and the
		// newly added IDs append in that displayed order.
		expect(mod.selectRange(['row-kilo'], DISPLAYED, 'row-kilo', 'row-alpha')).toEqual([
			'row-kilo',
			'row-alpha',
			'row-mike',
			'row-bravo'
		]);
	});

	it('RNG-U-03 Shift on the selected anchor row is a no-op, never a toggle-off', async () => {
		const mod = await requireModule('RNG-U-03');

		const selection = ['row-mike'];
		expect(mod.selectRange(selection, DISPLAYED, 'row-mike', 'row-mike')).toBe(selection);
	});

	it('RNG-U-04 Shift on the unselected anchor row adds that single row', async () => {
		const mod = await requireModule('RNG-U-04');

		expect(mod.selectRange([], DISPLAYED, 'row-mike', 'row-mike')).toEqual(['row-mike']);
	});

	it('RNG-U-13 already-selected middles are skipped and survivors keep activation order', async () => {
		const mod = await requireModule('RNG-U-13');

		// Activation order (bravo, zulu) survives; the zulu..kilo run appends
		// only alpha, mike and kilo in displayed-ascending order.
		expect(
			mod.selectRange(['row-bravo', 'row-zulu'], DISPLAYED, 'row-zulu', 'row-kilo')
		).toEqual(['row-bravo', 'row-zulu', 'row-alpha', 'row-mike', 'row-kilo']);
	});

	it('RNG-U-14 IDs compare exactly: case differs, span stays contiguous', async () => {
		const mod = await requireModule('RNG-U-14');

		const displayed = ['row-alpha', 'ROW-ALPHA', 'row-mike'];
		expect(mod.selectRange([], displayed, 'row-alpha', 'row-mike')).toEqual([
			'row-alpha',
			'ROW-ALPHA',
			'row-mike'
		]);
		// A wrong-case anchor is not displayed, so the focus plain-toggles.
		expect(mod.selectRange([], DISPLAYED, 'ROW-ALPHA', 'row-mike')).toEqual(['row-mike']);
	});
});

describe('anchor reset falls back to a plain toggle', () => {
	it('RNG-U-05 a null anchor toggles an unselected focus on', async () => {
		const mod = await requireModule('RNG-U-05');

		expect(mod.selectRange(['row-alpha'], DISPLAYED, null, 'row-mike')).toEqual([
			'row-alpha',
			'row-mike'
		]);
	});

	it('RNG-U-06 a null anchor toggles a selected focus off (all occurrences)', async () => {
		const mod = await requireModule('RNG-U-06');

		expect(
			mod.selectRange(['row-mike', 'row-alpha', 'row-mike'], DISPLAYED, null, 'row-mike')
		).toEqual(['row-alpha']);
	});

	it.each([[undefined], [42], [true], [{}], [[]], ['']])(
		'RNG-U-07 an invalid anchor %s toggles the focus',
		async (anchor) => {
			const mod = await requireModule('RNG-U-07');

			expect(
				mod.selectRange(
					['row-alpha'],
					DISPLAYED,
					anchor as unknown as string | null,
					'row-mike'
				)
			).toEqual(['row-alpha', 'row-mike']);
		}
	);

	it('RNG-U-08 an anchor absent from the displayed list toggles the focus', async () => {
		const mod = await requireModule('RNG-U-08');

		// The anchor-prune / anchor-hidden model: anchor gone from the visible
		// rows, so the gesture degrades to a plain toggle of the focus.
		expect(
			mod.selectRange(['row-alpha'], ['row-alpha', 'row-mike'], 'row-gone', 'row-mike')
		).toEqual(['row-alpha', 'row-mike']);
	});
});

describe('hidden rows are never touched', () => {
	it('RNG-U-09 a focus absent from the displayed list is a no-op', async () => {
		const mod = await requireModule('RNG-U-09');

		const selection = ['row-alpha'];
		expect(
			mod.selectRange(selection, ['row-alpha', 'row-mike'], 'row-alpha', 'row-gone')
		).toBe(selection);
	});

	it.each([[null], [undefined], [42], [true], [{}], [[]], ['']])(
		'RNG-U-10 an invalid focus %s is a no-op even with a usable anchor',
		async (focus) => {
			const mod = await requireModule('RNG-U-10');

			const selection = ['row-alpha'];
			expect(mod.selectRange(selection, DISPLAYED, 'row-alpha', focus)).toBe(selection);
		}
	);

	it('RNG-U-11 invalid displayed entries are ignored for span computation', async () => {
		const mod = await requireModule('RNG-U-11');

		const displayed = ['row-alpha', null, '', 42, 'row-mike'] as unknown as readonly string[];
		expect(mod.selectRange([], displayed, 'row-alpha', 'row-mike')).toEqual([
			'row-alpha',
			'row-mike'
		]);
	});

	it('RNG-U-12 duplicate displayed IDs resolve spans by first occurrence', async () => {
		const mod = await requireModule('RNG-U-12');

		const displayed = ['row-alpha', 'row-mike', 'row-alpha', 'row-bravo'];
		expect(mod.selectRange([], displayed, 'row-alpha', 'row-bravo')).toEqual([
			'row-alpha',
			'row-mike',
			'row-bravo'
		]);
	});

	it('RNG-U-17 an empty displayed list is a no-op', async () => {
		const mod = await requireModule('RNG-U-17');

		const selection = ['row-alpha'];
		expect(mod.selectRange(selection, [], 'row-alpha', 'row-mike')).toBe(selection);
	});
});

describe('range purity', () => {
	it('RNG-U-15 range application never mutates its inputs', async () => {
		const mod = await requireModule('RNG-U-15');

		const selection = ['row-bravo', 'row-zulu'];
		const displayed = [...DISPLAYED];
		const beforeSelection = JSON.stringify(selection);
		const beforeDisplayed = JSON.stringify(displayed);

		const ranged = mod.selectRange(selection, displayed, 'row-zulu', 'row-kilo');

		expect(JSON.stringify(selection)).toBe(beforeSelection);
		expect(JSON.stringify(displayed)).toBe(beforeDisplayed);
		expect(ranged).not.toBe(selection);
		expect(ranged).toEqual(['row-bravo', 'row-zulu', 'row-alpha', 'row-mike', 'row-kilo']);
	});

	it('RNG-U-16 invalid selection entries pass through untouched', async () => {
		const mod = await requireModule('RNG-U-16');

		const selection = ['row-alpha', '', 42] as unknown as readonly string[];
		expect(mod.selectRange(selection, DISPLAYED, 'row-alpha', 'row-mike')).toEqual([
			'row-alpha',
			'',
			42,
			'row-mike'
		]);
	});

	it('RNG-U-18 the primitive runs with no window, storage or network globals', async () => {
		const mod = await requireModule('RNG-U-18');

		expect(() => mod.selectRange([], DISPLAYED, 'row-alpha', 'row-kilo')).not.toThrow();
		expect(() => mod.selectRange(['row-alpha'], DISPLAYED, null, 'row-mike')).not.toThrow();
		expect(() => mod.selectRange(['row-alpha'], DISPLAYED, 'row-alpha', 'row-gone')).not.toThrow();
		expect(() => mod.selectRange([], [], null, 'row-mike')).not.toThrow();
	});
});
