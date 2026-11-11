import { render } from 'svelte/server';
import { describe, expect, it, vi } from 'vitest';

// Hoisted module mock: an invalid (empty) inventory. Kept in its own file so
// the component imports the mock statically — re-importing the component
// after `vi.resetModules()` gives it a different Svelte runtime than the
// test's `render`, which breaks SSR snippet/head rendering.
vi.mock('#lib/attributions/index.js', () => ({
	attributionInventory: { schemaVersion: 1, entries: [] },
	activeAttributions: [],
	plannedAttributions: [],
	fixtureAudioLicenceRecord: 'docs/fixture-audio-licences.md',
	validateAttributionInventory: (value: unknown) => {
		// Same shape as the real validator: empty entries are invalid.
		const entries = (value as { entries?: unknown[] })?.entries;
		return Array.isArray(entries) && entries.length > 0
			? []
			: ['entries must be a non-empty array'];
	}
}));

import Settings from './+page.svelte';

describe('settings About error state (invalid inventory)', () => {
	it('renders an announced error instead of a partial list', () => {
		const rendered = render(Settings);
		const about = rendered.body.slice(rendered.body.indexOf('id="about-nntune"'));
		expect(about).toContain('role="alert"');
		expect(about).toContain('Attribution data is unavailable.');
		expect(about).not.toContain('data-attribution-id=');
		// Provenance ships only alongside a valid render.
		expect(rendered.head).not.toContain('nntune-attribution-inventory');
		expect(rendered.body).not.toContain('nntune-attribution-inventory');
	});
});
