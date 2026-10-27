import { describe, expect, it } from 'vitest';
import { buildShareQuery, resolveShareTarget } from '../player/shareTimestamp.js';

/**
 * Test-Author owned RED acceptance for LIBSHARE: copy a library track link
 * that reopens the track at its position. The Builder wires
 * `apps/web/src/routes/library/+page.svelte` to satisfy this file plus
 * `apps/web/tests/library-share-link.spec.ts` and must not edit either file.
 *
 * LOCKED CONTRACT (`apps/web/src/routes/library/+page.svelte`):
 * - Every library row (`library-item`) carries a NATIVE <button>
 *   `library-copy-link` whose accessible name matches /copy link/i and names
 *   the row's track title (per-track naming, like the star toggles).
 * - `library-share-status`: one shared aria-live="polite" region (or
 *   role="status"). A fulfilled copy announces a concise (<100 chars)
 *   confirmation matching /copied/i and naming the copied track. It never
 *   claims success on fallback/refusal, clears on selection change and on
 *   fresh mount, and ignores late completion from a superseded action.
 * - `library-share-fallback`: one shared LABELED readonly <input> or
 *   <textarea> holding the share URL as its value. Shown only when the
 *   Clipboard API is absent or rejects. Focus stays on the activating row
 *   button (no focus steal).
 * - `library-share-instructions`: actionable manual-copy instructions shown
 *   with the fallback (matches /copy/i, names a manual gesture). Never
 *   claims success.
 * - `library-share-invalid`: invalid-link notice with role="alert" matching
 *   /invalid link/i, shown when `?track=`/`t=` fail to resolve against the
 *   loaded items. Selection, queue, media position and storage are untouched.
 * - Copy/open reuse `buildShareQuery` (copy) and `resolveShareTarget`
 *   (open) as the single encoding/timestamp sources: floor +
 *   encodeURIComponent on build, never-partial (track AND t) on resolve.
 * - Never calls legacy `document.execCommand` (no automatic fallback).
 * - Selection carries a loaded item's own `streamUrl` (when it is a usable
 *   non-blank string) into the queue so the existing `audio-player` renders;
 *   items without one keep today's select-only behavior (no audio).
 * See the Playwright spec header for the full page contract.
 *
 * PROTECTED (Test-Author owned; Builder must not edit):
 * - `apps/web/src/lib/library/shareLink.acceptance.test.ts` (this file)
 * - `apps/web/tests/library-share-link.spec.ts`
 * BUILDER-OWNED implementation surface:
 * - `apps/web/src/routes/library/+page.svelte` (copy buttons, status,
 *   fallback, invalid notice, open-link select+seek, streamUrl carry-through)
 * - Existing share/seek suites (`shareTimestamp.*`, `shareCopy.*`,
 *   `player-comforts.spec.ts`, `player-share-copy.spec.ts`) stay unchanged.
 *
 * Run from the repository root (Node 24 / pnpm 10), offline:
 *   pnpm --dir apps/web test src/lib/library/shareLink.acceptance.test.ts
 * Companion browser run (builds + serves locally; no staging/prod):
 *   CI=1 pnpm --dir apps/web test:e2e tests/library-share-link.spec.ts --workers=1
 * Do not insert `--` after these pnpm scripts: this toolchain forwards it to
 * Vitest/Playwright, ending option parsing and selecting the entire suite.
 *
 * RED NOW: the library route has no share hooks and imports neither
 * `buildShareQuery` nor `resolveShareTarget`. GREEN CONTROLS: the
 * no-execCommand pin and the helper-contract pins pass on the current tree,
 * proving this file compiles and the reuse contract holds.
 * Every RED assertion message is a compact JSON receipt:
 * {"case","expected","actual","status","error_code","next_action"}.
 */

// Raw route markup via Vite glob: no node:fs, no @types/node needed.
const sources = import.meta.glob('../../routes/**/*.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

const ROUTE = '../../routes/library/+page.svelte';

function routeSource(): string {
	const source = sources[ROUTE];
	expect(
		source,
		JSON.stringify({
			case: 'LIBSHARE-ROUTE',
			expected: `route source "${ROUTE}" to exist`,
			actual: 'missing',
			status: 'RED',
			error_code: 'route_missing',
			next_action: 'Do not move the library route; Builder adds share hooks there.'
		})
	).toBeDefined();
	return source ?? '';
}

/** Compact JSON receipt carried by every RED assertion message. */
function note(
	caseId: string,
	expected: string,
	actual: string,
	errorCode: string,
	nextAction: string
): string {
	return JSON.stringify({
		case: caseId,
		expected,
		actual,
		status: 'RED',
		error_code: errorCode,
		next_action: nextAction
	});
}

/** Library-shaped ids (never the demo catalogue): reuse must be id-agnostic. */
const LIBRARY_IDS = ['share-coral', 'share-pine', 'share-ember'];

describe('library share hooks are declared in the library markup', () => {
	it('LIBSHARE-U01 declares a per-row copy-link action (library-copy-link)', () => {
		expect(
			routeSource(),
			note(
				'LIBSHARE-U01',
				'data-testid="library-copy-link" in the library markup',
				'missing',
				'copy_action_missing',
				'Builder adds a native per-row <button data-testid="library-copy-link">Copy link <title></button> (see library-share-link.spec.ts LOCKED CONTRACT).'
			)
		).toContain('data-testid="library-copy-link"');
	});

	it('LIBSHARE-U02 declares the polite share status region (library-share-status)', () => {
		expect(
			routeSource(),
			note(
				'LIBSHARE-U02',
				'data-testid="library-share-status" in the library markup',
				'missing',
				'status_region_missing',
				'Builder adds the shared aria-live polite confirmation region.'
			)
		).toContain('data-testid="library-share-status"');
	});

	it('LIBSHARE-U03 declares the readonly fallback URL (library-share-fallback)', () => {
		expect(
			routeSource(),
			note(
				'LIBSHARE-U03',
				'data-testid="library-share-fallback" in the library markup',
				'missing',
				'fallback_missing',
				'Builder adds the labeled readonly fallback input shown when the Clipboard API is absent or rejects.'
			)
		).toContain('data-testid="library-share-fallback"');
	});

	it('LIBSHARE-U04 declares actionable copy instructions (library-share-instructions)', () => {
		expect(
			routeSource(),
			note(
				'LIBSHARE-U04',
				'data-testid="library-share-instructions" in the library markup',
				'missing',
				'instructions_missing',
				'Builder adds manual copy instructions shown with the fallback (no success claim).'
			)
		).toContain('data-testid="library-share-instructions"');
	});

	it('LIBSHARE-U05 declares the invalid-link notice (library-share-invalid)', () => {
		expect(
			routeSource(),
			note(
				'LIBSHARE-U05',
				'data-testid="library-share-invalid" in the library markup',
				'missing',
				'invalid_notice_missing',
				'Builder adds the role=alert invalid-link notice shown when ?track=/t= fail to resolve.'
			)
		).toContain('data-testid="library-share-invalid"');
	});
});

describe('library share encoding source', () => {
	it('LIBSHARE-U06 reuses buildShareQuery as the single copy-encoding source', () => {
		expect(
			routeSource(),
			note(
				'LIBSHARE-U06',
				'the library route reuses buildShareQuery',
				'no reference',
				'build_reuse_missing',
				'Builder imports buildShareQuery from #lib/player/shareTimestamp.js and builds the copied query with it (no cloned search/hash, no stream URL).'
			)
		).toContain('buildShareQuery');
	});

	it('LIBSHARE-U07 reuses resolveShareTarget as the single open-link source', () => {
		expect(
			routeSource(),
			note(
				'LIBSHARE-U07',
				'the library route reuses resolveShareTarget',
				'no reference',
				'resolve_reuse_missing',
				'Builder imports resolveShareTarget from #lib/player/shareTimestamp.js and resolves ?track=/t= against loaded item ids with it (never partial).'
			)
		).toContain('resolveShareTarget');
	});

	it('LIBSHARE-U08 never uses a legacy execCommand fallback (control: passes now, must stay green)', () => {
		expect(
			routeSource(),
			note(
				'LIBSHARE-U08',
				'no document.execCommand in the library route',
				'execCommand present',
				'exec_command_forbidden',
				'Remove the legacy automatic fallback; clipboard absence/rejection shows the readonly fallback instead.'
			)
		).not.toContain('execCommand');
	});
});

describe('reused helper contract with library-shaped ids (controls: pass now, must stay green)', () => {
	it('LIBSHARE-U09 build/parse round-trips library ids with floor + encode semantics', () => {
		// The Playwright spec asserts copied/opened URLs equal these shapes.
		expect(buildShareQuery('share-pine', 95.9)).toBe('track=share-pine&t=95');
		expect(buildShareQuery('share-coral', 0)).toBe('track=share-coral&t=0');
		expect(buildShareQuery('a b&c', 5)).toBe('track=a%20b%26c&t=5');
		expect(() => buildShareQuery('share-coral', Number.NaN)).toThrow(RangeError);
		expect(() => buildShareQuery('share-coral', -1)).toThrow(RangeError);
		expect(() => buildShareQuery('', 10)).toThrow(RangeError);

		const query = buildShareQuery('share-ember', 125.7);
		const params = new URLSearchParams(query);
		expect(resolveShareTarget({ track: params.get('track'), t: params.get('t') }, LIBRARY_IDS)).toEqual(
			{ trackId: 'share-ember', seconds: 125 }
		);
	});

	it('LIBSHARE-U10 resolveShareTarget is never partial against loaded ids', () => {
		expect(resolveShareTarget({ track: 'share-pine', t: '1:30' }, LIBRARY_IDS)).toEqual({
			trackId: 'share-pine',
			seconds: 90
		});
		// Fractional input keeps its fraction on open (demo inbound parity).
		expect(resolveShareTarget({ track: 'share-pine', t: '77.8' }, LIBRARY_IDS)).toEqual({
			trackId: 'share-pine',
			seconds: 77.8
		});
		// Unknown ids never resolve, even with a valid t — including ids from
		// outside the loaded library (demo catalogue, other accounts).
		for (const track of [null, '', 'no-such-track', 'demo-track', ' Share-Pine ']) {
			expect(resolveShareTarget({ track, t: '90' }, LIBRARY_IDS)).toBeNull();
		}
		// Unparsable or missing t never resolves, even with a valid track.
		for (const t of [null, '', 'soon', '-3']) {
			expect(resolveShareTarget({ track: 'share-coral', t }, LIBRARY_IDS)).toBeNull();
		}
		expect(resolveShareTarget({ track: 'share-coral', t: '90' }, [])).toBeNull();
	});
});
