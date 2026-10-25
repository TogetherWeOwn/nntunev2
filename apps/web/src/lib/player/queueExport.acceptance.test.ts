import { describe, expect, it } from 'vitest';
import {
	DEMO_TRACKS,
	createQueue,
	moveTrack,
	removeTrack,
	selectTrack,
	type QueueState,
	type Track
} from './queue.js';
import * as queueModule from './queue.js';

/**
 * Test-Author owned RED acceptance for QUEUEEXPORT: copy or download the
 * arranged queue as a track list. The Builder adds the pure formatters to
 * `queue.ts` and wires `/demo/player` to satisfy this file plus
 * `apps/web/tests/player-queue-export.spec.ts`, and must not edit either file.
 *
 * LOCKED CONTRACT (`apps/web/src/lib/player/queue.ts`):
 * - `formatQueueText(state: QueueState): string` renders one line per track
 *   in track-list order: `N. <title>` plus ` — <artist>` (U+2014 em dash,
 *   matching the on-screen queue rows) when the artist is non-blank after
 *   sanitizing. Lines join with `\n` (no trailing newline); an empty queue
 *   renders as `''`.
 * - `formatQueueM3U(state: QueueState): string` renders `#EXTM3U`, then per
 *   track `#EXTINF:-1,<title>[ — <artist>]` plus a bare stable track-id line.
 *   Lines join with `\n` (no trailing newline); an empty queue renders as
 *   the bare `#EXTM3U` header. Ids only: never stream URLs, signed grants,
 *   provider info, or identity.
 * - Sanitizing runs before fallbacks: every maximal run of C0 controls or
 *   DEL (`[\0-\x1F\x7F]+`) becomes one space and the field is trimmed, so
 *   newlines/tabs/NUL can never break the one-line-per-track shape. A blank
 *   title falls back to the track id; a blank artist is omitted (matching
 *   the on-screen queue rows). Unicode is preserved verbatim. Selection
 *   (`currentIndex`) never changes either rendering. Both formatters are pure
 *   and offline: no clipboard, storage, network, or playback access.
 *
 * LOCKED CONTRACT (`apps/web/src/routes/demo/player/+page.svelte`, mirroring
 * the share-copy pattern; see the Playwright spec for behavior):
 * - Labeled native actions `queue-export-copy` ("Copy queue as text") and
 *   `queue-export-download` ("Download queue.m3u"), a polite status region
 *   `queue-export-status`, a labeled readonly fallback `queue-export-fallback`
 *   holding the track-list text, and actionable instructions
 *   `queue-export-instructions`.
 * - The route reuses `formatQueueText`/`formatQueueM3U` as the single export
 *   source (no cloned formatting, no stream URL) and never calls legacy
 *   `document.execCommand`.
 *
 * RED NOW: `queue.ts` has no export formatters and the route has no export
 * hooks, so every T/M/H case below fails at its assertion with a NEXT action
 * (namespace access keeps this file compiling: no static import of the
 * missing exports). GREEN CONTROLS: the C-cases and the no-execCommand pin
 * pass on the current tree, proving this file compiles and the queue helpers
 * it drives are real.
 */

// The export formatters do not exist yet: namespace access (never a static
// named import) keeps this file compiling on the current tree, so every case
// below fails as an assertion — never an import error.
type QueueExporters = {
	formatQueueText?: unknown;
	formatQueueM3U?: unknown;
};

const { formatQueueText, formatQueueM3U } = queueModule as unknown as QueueExporters;

type FormatQueue = (state: QueueState) => string;

const MISSING_FORMATTERS =
	'QUEUEEXPORT: expected queue.ts to export the pure queue-export formatters (actual: missing). ' +
	'NEXT: Builder adds formatQueueText/formatQueueM3U to apps/web/src/lib/player/queue.ts per the ' +
	'LOCKED CONTRACT above (pure, offline, no stream URLs); this file stays untouched.';

function exporters(): { formatQueueText: FormatQueue; formatQueueM3U: FormatQueue } {
	expect(typeof formatQueueText, `${MISSING_FORMATTERS} (formatQueueText)`).toBe('function');
	expect(typeof formatQueueM3U, `${MISSING_FORMATTERS} (formatQueueM3U)`).toBe('function');
	return {
		formatQueueText: formatQueueText as FormatQueue,
		formatQueueM3U: formatQueueM3U as FormatQueue
	};
}

function track(id: string, title: string, artist: string, streamUrl?: string): Track {
	return {
		id,
		title,
		artist,
		streamUrl: streamUrl ?? `/api/demo/audio?track=${id}`,
		artworkUrl: null
	};
}

// Deliberately non-alphabetical id/title order, and ids that do not sort the
// same way as the list order, so a formatter that sorts (or renders a
// hard-coded fixture) instead of following the given track list fails.
const FIRST = track('track-mango', 'Mango Season', 'Acceptance Band');
const SECOND = track('track-apple', 'Apple Cart', 'Orchard Union');
const THIRD = track('track-zebra', 'Zebra Crossing', 'Session Players');
const TRACKS = [FIRST, SECOND, THIRD];

// Raw route markup via Vite glob: no node:fs, no @types/node needed.
const sources = import.meta.glob('../../routes/**/*.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

const ROUTE = '../../routes/demo/player/+page.svelte';

function routeSource(): string {
	const source = sources[ROUTE];
	expect(
		source,
		`QUEUEEXPORT: expected route source "${ROUTE}" to exist. NEXT: do not move the demo player route; Builder adds the queue-export actions there.`
	).toBeDefined();
	return source ?? '';
}

describe('formatQueueText', () => {
	it('[QUEUEEXPORT-T01] numbers "title — artist" lines in track-list order', () => {
		const { formatQueueText } = exporters();

		expect(formatQueueText(createQueue(TRACKS))).toBe(
			'1. Mango Season — Acceptance Band\n' +
				'2. Apple Cart — Orchard Union\n' +
				'3. Zebra Crossing — Session Players'
		);
	});

	it('[QUEUEEXPORT-T02] omits the artist separator when the artist is missing or blank', () => {
		const { formatQueueText } = exporters();
		const state = createQueue([
			track('track-solo', 'Solo Flight', ''),
			track('track-blank', 'Blank Harmony', '   ')
		]);

		expect(formatQueueText(state)).toBe('1. Solo Flight\n2. Blank Harmony');
	});

	it('[QUEUEEXPORT-T03] falls back to the track id when the title is missing, blank, or only controls', () => {
		const { formatQueueText } = exporters();
		const state = createQueue([
			track('track-no-title', '', 'Ghost Choir'),
			track('track-spaces', '   ', 'Echoes'),
			track('track-control-only', '\n\r\0', 'Static Bloom')
		]);

		expect(formatQueueText(state)).toBe(
			'1. track-no-title — Ghost Choir\n' +
				'2. track-spaces — Echoes\n' +
				'3. track-control-only — Static Bloom'
		);
	});

	it('[QUEUEEXPORT-T04] strips newlines and control characters so each track stays on one line', () => {
		const { formatQueueText } = exporters();
		const state = createQueue([track('track-inject', 'One\nTwo\r\nThree\tEnd', 'A\0B\x01C\x7FD')]);

		const text = formatQueueText(state);
		expect(text).toBe('1. One Two Three End — A B C D');
		expect(text.split('\n')).toHaveLength(1);
		expect(text, 'exported text must not contain control characters').not.toMatch(/[\0-\x1F\x7F]/);
	});

	it('[QUEUEEXPORT-T05] preserves unicode titles and artists verbatim', () => {
		const { formatQueueText } = exporters();
		const state = createQueue([
			track('track-unicode', 'Björk — Guðmundsdóttir 🦢', 'Sigur Rós 日本語')
		]);

		expect(formatQueueText(state)).toBe('1. Björk — Guðmundsdóttir 🦢 — Sigur Rós 日本語');
	});

	it('[QUEUEEXPORT-T06] renders an empty queue as an empty string', () => {
		const { formatQueueText } = exporters();

		expect(formatQueueText(createQueue([]))).toBe('');
	});

	it('[QUEUEEXPORT-T07] follows the arranged order after move and remove', () => {
		const { formatQueueText } = exporters();
		let state = moveTrack(createQueue(TRACKS), 2, 0);

		expect(formatQueueText(state)).toBe(
			'1. Zebra Crossing — Session Players\n' +
				'2. Mango Season — Acceptance Band\n' +
				'3. Apple Cart — Orchard Union'
		);

		state = removeTrack(state, 1);
		expect(formatQueueText(state)).toBe(
			'1. Zebra Crossing — Session Players\n2. Apple Cart — Orchard Union'
		);
	});

	it('[QUEUEEXPORT-T08] ignores selection: the same text whatever the current index', () => {
		const { formatQueueText } = exporters();
		const expected = formatQueueText(createQueue(TRACKS));

		expect(formatQueueText(selectTrack(createQueue(TRACKS), 0))).toBe(expected);
		expect(formatQueueText(selectTrack(createQueue(TRACKS), 2))).toBe(expected);
	});

	it('[QUEUEEXPORT-T09] never emits stream URLs, grants, provider info, or identity', () => {
		const { formatQueueText } = exporters();
		const state = createQueue([
			...TRACKS,
			track(
				'track-signed',
				'Signed Stream',
				'Grant Ensemble',
				'https://stream.example/track?grant=SIGNED_GRANT_9&token=TOK_1&provider=acme'
			)
		]);

		const text = formatQueueText(state);
		for (const leaked of [
			'/api/demo/audio',
			'stream.example',
			'SIGNED_GRANT_9',
			'TOK_1',
			'provider=acme',
			'https://'
		]) {
			expect(text, `exported text must not contain "${leaked}"`).not.toContain(leaked);
		}
	});
});

describe('formatQueueM3U', () => {
	it('[QUEUEEXPORT-M01] renders #EXTM3U with #EXTINF entries and bare track-id lines in order', () => {
		const { formatQueueM3U } = exporters();

		expect(formatQueueM3U(createQueue(TRACKS))).toBe(
			'#EXTM3U\n' +
				'#EXTINF:-1,Mango Season — Acceptance Band\n' +
				'track-mango\n' +
				'#EXTINF:-1,Apple Cart — Orchard Union\n' +
				'track-apple\n' +
				'#EXTINF:-1,Zebra Crossing — Session Players\n' +
				'track-zebra'
		);
	});

	it('[QUEUEEXPORT-M02] applies the same title/artist fallbacks as the text export', () => {
		const { formatQueueM3U } = exporters();
		const state = createQueue([
			track('track-solo', 'Solo Flight', ''),
			track('track-no-title', '', 'Ghost Choir')
		]);

		expect(formatQueueM3U(state)).toBe(
			'#EXTM3U\n#EXTINF:-1,Solo Flight\ntrack-solo\n#EXTINF:-1,track-no-title — Ghost Choir\ntrack-no-title'
		);
	});

	it('[QUEUEEXPORT-M03] strips newlines and control characters from entries', () => {
		const { formatQueueM3U } = exporters();
		const state = createQueue([track('track-inject', 'One\nTwo\r\nThree\tEnd', 'A\0B\x01C\x7FD')]);

		const m3u = formatQueueM3U(state);
		expect(m3u).toBe('#EXTM3U\n#EXTINF:-1,One Two Three End — A B C D\ntrack-inject');
		// Newlines separate entries; every other control byte is forbidden.
		expect(m3u, 'M3U bytes must not contain control characters').not.toMatch(
			/[\0-\x09\x0B-\x1F\x7F]/
		);
	});

	it('[QUEUEEXPORT-M04] preserves unicode in entries', () => {
		const { formatQueueM3U } = exporters();
		const state = createQueue([
			track('track-unicode', 'Björk — Guðmundsdóttir 🦢', 'Sigur Rós 日本語')
		]);

		expect(formatQueueM3U(state)).toBe(
			'#EXTM3U\n#EXTINF:-1,Björk — Guðmundsdóttir 🦢 — Sigur Rós 日本語\ntrack-unicode'
		);
	});

	it('[QUEUEEXPORT-M05] renders an empty queue as the bare #EXTM3U header', () => {
		const { formatQueueM3U } = exporters();

		expect(formatQueueM3U(createQueue([]))).toBe('#EXTM3U');
	});

	it('[QUEUEEXPORT-M06] follows the arranged order after move and remove', () => {
		const { formatQueueM3U } = exporters();
		let state = moveTrack(createQueue(TRACKS), 2, 0);

		expect(formatQueueM3U(state)).toBe(
			'#EXTM3U\n' +
				'#EXTINF:-1,Zebra Crossing — Session Players\n' +
				'track-zebra\n' +
				'#EXTINF:-1,Mango Season — Acceptance Band\n' +
				'track-mango\n' +
				'#EXTINF:-1,Apple Cart — Orchard Union\n' +
				'track-apple'
		);

		state = removeTrack(state, 1);
		expect(formatQueueM3U(state)).toBe(
			'#EXTM3U\n' +
				'#EXTINF:-1,Zebra Crossing — Session Players\n' +
				'track-zebra\n' +
				'#EXTINF:-1,Apple Cart — Orchard Union\n' +
				'track-apple'
		);
	});

	it('[QUEUEEXPORT-M07] references stable track ids only: no URLs, grants, or providers', () => {
		const { formatQueueM3U } = exporters();
		const state = createQueue([
			...TRACKS,
			track(
				'track-signed',
				'Signed Stream',
				'Grant Ensemble',
				'https://stream.example/track?grant=SIGNED_GRANT_9&token=TOK_1&provider=acme'
			)
		]);

		const m3u = formatQueueM3U(state);
		const lines = m3u.split('\n');
		expect(lines[0]).toBe('#EXTM3U');
		// Even lines after the header are bare ids (odd lines are #EXTINF entries).
		expect(lines.filter((_, i) => i % 2 === 0).slice(1)).toEqual([
			'track-mango',
			'track-apple',
			'track-zebra',
			'track-signed'
		]);
		for (const leaked of [
			'/api/demo/audio',
			'stream.example',
			'SIGNED_GRANT_9',
			'TOK_1',
			'provider=acme',
			'https://'
		]) {
			expect(m3u, `M3U bytes must not contain "${leaked}"`).not.toContain(leaked);
		}
	});
});

describe('queue-export hooks are declared in the demo player markup', () => {
	it('[QUEUEEXPORT-H01] declares the labeled native copy action (queue-export-copy)', () => {
		expect(
			routeSource(),
			'QUEUEEXPORT: expected data-testid="queue-export-copy" in the demo player markup (actual: missing). NEXT: Builder adds a native <button data-testid="queue-export-copy">Copy queue as text</button> (see the player-queue-export spec LOCKED CONTRACT).'
		).toContain('data-testid="queue-export-copy"');
	});

	it('[QUEUEEXPORT-H02] declares the labeled native download action (queue-export-download)', () => {
		expect(
			routeSource(),
			'QUEUEEXPORT: expected data-testid="queue-export-download" in the demo player markup (actual: missing). NEXT: Builder adds a native <button data-testid="queue-export-download">Download queue.m3u</button> (see the player-queue-export spec LOCKED CONTRACT).'
		).toContain('data-testid="queue-export-download"');
	});

	it('[QUEUEEXPORT-H03] declares the polite status region (queue-export-status)', () => {
		expect(
			routeSource(),
			'QUEUEEXPORT: expected data-testid="queue-export-status" in the demo player markup (actual: missing). NEXT: Builder adds the aria-live polite confirmation region (copy/download success, empty-queue reason; never a false success claim).'
		).toContain('data-testid="queue-export-status"');
	});

	it('[QUEUEEXPORT-H04] declares the readonly fallback text (queue-export-fallback)', () => {
		expect(
			routeSource(),
			'QUEUEEXPORT: expected data-testid="queue-export-fallback" in the demo player markup (actual: missing). NEXT: Builder adds the labeled readonly fallback input holding the track-list text when the Clipboard API is absent or rejects.'
		).toContain('data-testid="queue-export-fallback"');
	});

	it('[QUEUEEXPORT-H05] declares actionable export instructions (queue-export-instructions)', () => {
		expect(
			routeSource(),
			'QUEUEEXPORT: expected data-testid="queue-export-instructions" in the demo player markup (actual: missing). NEXT: Builder adds manual copy instructions shown with the fallback (no success claim).'
		).toContain('data-testid="queue-export-instructions"');
	});
});

describe('queue-export source contract', () => {
	it('[QUEUEEXPORT-H06] reuses formatQueueText/formatQueueM3U as the single export source', () => {
		const source = routeSource();
		expect(
			source,
			'QUEUEEXPORT: expected the demo player to reuse formatQueueText (actual: no reference). NEXT: Builder imports formatQueueText from the player queue module and builds the copied text with it (no cloned formatting, no stream URL).'
		).toContain('formatQueueText');
		expect(
			source,
			'QUEUEEXPORT: expected the demo player to reuse formatQueueM3U (actual: no reference). NEXT: Builder imports formatQueueM3U from the player queue module and builds the downloaded bytes with it (no cloned formatting, no stream URL).'
		).toContain('formatQueueM3U');
	});

	it('[QUEUEEXPORT-H07] never uses a legacy execCommand fallback (control: passes now, must stay green)', () => {
		expect(
			routeSource(),
			'QUEUEEXPORT: the demo player must never call document.execCommand (no legacy automatic fallback).'
		).not.toContain('execCommand');
	});
});

describe('queue-export controls (pass now, must stay green)', () => {
	it('[QUEUEEXPORT-C01] demo catalogue ids are stable', () => {
		expect(DEMO_TRACKS.map((t) => t.id)).toEqual(['demo-track', 'second-signal', 'third-light']);
	});

	it('[QUEUEEXPORT-C02] moveTrack keeps the current track while reordering', () => {
		const moved = moveTrack(createQueue(TRACKS), 2, 0);
		expect(moved.tracks.map((t) => t.id)).toEqual(['track-zebra', 'track-mango', 'track-apple']);
		expect(moved.currentIndex).toBe(1);
	});
});
