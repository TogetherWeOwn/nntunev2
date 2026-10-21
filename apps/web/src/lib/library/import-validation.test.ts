import { describe, expect, it } from 'vitest';

import { MAX_NZB_IMPORT_BYTES, validateImportFile } from './import-validation.js';

/**
 * Acceptance: client-side NZB import file validation + honest library UI.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build card implements
 * `./import-validation.ts` and the route changes on top of this branch and
 * must make these tests pass WITHOUT editing this file; a wrong test comes
 * back to the acceptance card instead.
 *
 * Validator contract the build must provide (used by
 * `routes/library/import/+page.svelte` before any network call):
 * - `MAX_NZB_IMPORT_BYTES`: the single exported size cap. It mirrors the
 *   ingest Worker's intake cap (`MAX_NZB_BYTES = 2 * 1024 * 1024` in
 *   `workers/ingest/src/nzb_intake.rs`) — cross-reference that source in a
 *   comment next to the constant so the two caps cannot drift silently.
 * - `validateImportFile(file): string | null` — `null` when the file is
 *   acceptable, otherwise a non-empty user-facing message the import page
 *   can show verbatim in its `role="alert"` element. It rejects zero-byte
 *   files, files over the cap, and names that do not end in `.nzb`
 *   (case-insensitive). Exact wording is the builder's: these tests pin
 *   the outcome and the topic of the message, not its literal text.
 */
function file(name: string, size: number): File {
	return new File([new Uint8Array(size)], name);
}

describe('MAX_NZB_IMPORT_BYTES', () => {
	it("equals the ingest Worker's 2 MiB NZB intake cap", () => {
		// workers/ingest/src/nzb_intake.rs: `pub const MAX_NZB_BYTES: usize = 2 * 1024 * 1024;`
		expect(MAX_NZB_IMPORT_BYTES).toBe(2 * 1024 * 1024);
	});
});

describe('validateImportFile', () => {
	it('accepts a small well-named NZB file', () => {
		expect(validateImportFile(file('album.nzb', 1024))).toBeNull();
	});

	it('accepts an uppercase .NZB extension', () => {
		expect(validateImportFile(file('ALBUM.NZB', 1024))).toBeNull();
		expect(validateImportFile(file('Album.Nzb', 1024))).toBeNull();
	});

	it('accepts a file exactly at the 2 MiB cap', () => {
		expect(validateImportFile(file('album.nzb', MAX_NZB_IMPORT_BYTES))).toBeNull();
	});

	it('rejects a zero-byte file as empty', () => {
		const error = validateImportFile(file('album.nzb', 0));
		expect(error, 'zero-byte file must be rejected').not.toBeNull();
		expect(error).toMatch(/empty|no data|0 bytes/i);
	});

	it('rejects a file one byte over the cap as too large', () => {
		const error = validateImportFile(file('album.nzb', MAX_NZB_IMPORT_BYTES + 1));
		expect(error, 'over-cap file must be rejected').not.toBeNull();
		expect(error).toMatch(/too (large|big)|size|limit|exceed|maximum|2\s?(mb|mib)/i);
	});

	it('rejects files whose name does not end in .nzb', () => {
		for (const name of ['notes.txt', 'album.zip', 'album', 'album.nzb.txt', 'album.nz']) {
			const error = validateImportFile(file(name, 1024));
			expect(error, `rejected: ${name}`).not.toBeNull();
			expect(error, `message names the expected kind: ${name}`).toMatch(/nzb|extension|type/i);
		}
	});

	it('returns a displayable message, never an empty string, for every rejection', () => {
		for (const rejected of [
			file('album.nzb', 0),
			file('album.nzb', MAX_NZB_IMPORT_BYTES + 1),
			file('notes.txt', 1024)
		]) {
			const error = validateImportFile(rejected);
			expect(typeof error).toBe('string');
			expect((error as string).trim().length).toBeGreaterThan(0);
		}
	});
});

// Raw route markup via Vite glob (same seam as `../contract.test.ts`).
const routeSources = import.meta.glob('../../routes/**/*.svelte', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

describe('library player markup', () => {
	// The positive path (a selected track carrying a real stream URL) is not
	// reachable in this slice: the library API shape has no stream URL yet
	// and player wiring is explicitly out of scope. The Playwright half
	// pins the reachable behaviour (no <audio> at all while no real URL
	// exists); this source pin is the closest honest seam for "when a real
	// URL exists, the player renders with its contract hook and a bound
	// src, never a literal empty one".
	const source = routeSources['../../routes/library/+page.svelte'];

	it('keeps the audio-player contract hook on an <audio> element', () => {
		expect(source).toMatch(/<audio[^>]*data-testid="audio-player"/);
	});

	it('never renders a literal empty src — the src is bound to the stream URL', () => {
		expect(source).not.toMatch(/<audio[^>]*\ssrc=""/);
		expect(source).toMatch(/<audio[^>]*\ssrc=\{/);
	});
});
