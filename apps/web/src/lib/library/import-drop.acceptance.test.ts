import { describe, expect, it } from 'vitest';

import { dropCases } from '../../../tests/import-drop.cases.js';
import { validateImportFile } from './import-validation.js';

/**
 * Protected acceptance tests; the paired Builder adds production behavior, not
 * edits to these assertions. Browser/state-transition half: tests/import-drop.spec.ts.
 * Run: pnpm --dir apps/web test src/lib/library/import-drop.acceptance.test.ts
 *
 * These existing-validator controls intentionally stay GREEN. The real-page
 * browser half supplies assertion-RED for missing drop behavior. No missing
 * module, prospective export, stub implementation or raw-source assertion is used.
 */
describe('local NZB drop: authoritative metadata validation controls', () => {
	for (const fixture of dropCases) {
		it(`${fixture.id} validates metadata without reading File contents`, () => {
			// Break caught: validator reads contents/MIME instead of using only the
			// existing filename/size rules, loses a boundary or changes case handling.
			const file = new File([new Uint8Array(fixture.size)], fixture.name, {
				type: 'application/octet-stream'
			});
			for (const method of ['text', 'arrayBuffer', 'stream', 'slice', 'bytes']) {
				Object.defineProperty(file, method, {
					value: () => { throw new Error(`File content read via ${method}`); }
				});
			}
			Object.defineProperty(file, 'type', {
				get: () => { throw new Error('Validation must not depend on MIME type'); }
			});

			const error = validateImportFile(file);
			if (fixture.error === null) {
				expect(error).toBeNull();
			} else {
				expect(error).toMatch(fixture.error);
			}
		});
	}
});
