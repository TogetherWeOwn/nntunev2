import { render } from 'svelte/server';
import { describe, expect, it, vi, afterEach } from 'vitest';

import Settings from './+page.svelte';
import {
	activeAttributions,
	attributionInventory,
	plannedAttributions,
	validateAttributionInventory
} from '#lib/attributions/index.js';
import { APP_VERSION } from '#lib/version.js';
import { BUILD_SHA, isBuildShaKnown } from '#lib/build.js';

function body(): string {
	return render(Settings).body;
}

function head(): string {
	return render(Settings).head;
}

function aboutSection(html: string): string {
	const start = html.indexOf('id="about-nntune"');
	expect(start).toBeGreaterThan(-1);
	return html.slice(start);
}

function attributionIds(html: string): string[] {
	const about = aboutSection(html);
	return [...about.matchAll(/data-attribution-id="([^"]+)"/g)].map((match) => match[1] as string);
}

afterEach(() => {
	vi.doUnmock('#lib/attributions/index.js');
	vi.resetModules();
});

describe('settings About NNTune (licences/about)', () => {
	it('renders the About section with version and build suffix rules', () => {
		const html = body();
		const about = aboutSection(html);
		expect(about).toContain('About NNTune');
		expect(about).toContain(`Version ${APP_VERSION}`);
		if (isBuildShaKnown(BUILD_SHA)) {
			expect(about).toContain(`build ${BUILD_SHA.slice(0, 7)}`);
		} else {
			expect(about).not.toContain('build ');
		}
		// Heading order follows the existing sections: h2 then h3.
		expect(about.indexOf('<h2')).toBeLessThan(about.indexOf('<h3'));
		expect(about).toMatch(/<h2[^>]*id="settings-about-heading"/);
		expect(about).toMatch(/<h3[^>]*id="settings-about-licences-heading">Licences<\/h3>/);
	});

	it('renders active notices with source and licence links, never copying a banned brand', () => {
		const about = aboutSection(body());
		for (const entry of activeAttributions) {
			expect(about, entry.id).toContain(`data-attribution-id="${entry.id}" data-status="active"`);
			expect(about, entry.id).toContain(entry.name);
			expect(about, entry.id).toContain(entry.notice);
			expect(about, entry.id).toContain(entry.obligations);
			expect(about, entry.id).toContain(`href="${entry.sourceUrl}"`);
			expect(about, entry.id).toContain(`href="${entry.licenseUrl}"`);
		}
		// Original icon work, never a third-party icon-set credit.
		expect(about).not.toContain('Lucide');
		expect(about).toContain('NNTune uses original inline SVG stencils');
		// Approved licence intro lines from the design spec.
		expect(about).toContain('NNTune is open source under the MIT License.');
		expect(about).toContain('Typeface: Figtree, SIL Open Font License 1.1.');
	});

	it('links the shipped Open Font License copy same-origin', () => {
		const about = aboutSection(body());
		expect(about).toContain('href="/fonts/OFL.txt"');
		// The shipped copy is the only same-origin attribution link; every
		// other attribution href is an authoritative https URL.
		for (const href of [...about.matchAll(/<a href="([^"]+)"[^>]*>/g)].map((m) => m[1] as string)) {
			if (href === '/fonts/OFL.txt') continue;
			expect(href.startsWith('https://') || href.startsWith('/library') ||
				href.startsWith('/account'), href).toBe(true);
		}
	});

	it('keeps planned sources inactive and labelled, never presented as shipped data', () => {
		const about = aboutSection(body());
		const ids = attributionIds(body());
		expect(ids).toEqual(attributionInventory.entries.map((entry) => entry.id));
		for (const entry of plannedAttributions) {
			expect(about, entry.id).toContain(
				`data-attribution-id="${entry.id}" data-status="planned"`
			);
			expect(about, entry.id).toContain(entry.notice);
		}
		// Planned ids render only after the Planned heading, with planned
		// status, and never inside the active licence list.
		const plannedHeading = about.indexOf('Planned sources');
		expect(plannedHeading).toBeGreaterThan(-1);
		const licencePart = about.slice(0, plannedHeading);
		for (const entry of plannedAttributions) {
			expect(licencePart).not.toContain(`data-attribution-id="${entry.id}"`);
		}
		// No "Data sources" h3 while no data source is active.
		expect(about).not.toContain('settings-about-sources-heading');
		expect(
			plannedAttributions.every((entry) => entry.status === 'planned' && entry.kind === 'data' || entry.kind === 'artwork')
		).toBe(true);
	});

	it('embeds the machine-readable inventory verbatim alongside the lists', () => {
		const { head: pageHead } = render(Settings);
		const match = pageHead.match(/id="nntune-attribution-inventory">([\s\S]*?)<\/script>/);
		expect(match, 'provenance script present').not.toBeNull();
		const parsed = JSON.parse((match as RegExpMatchArray)[1] as string) as typeof attributionInventory;
		expect(validateAttributionInventory(parsed)).toEqual([]);
		expect(parsed).toEqual(attributionInventory);
		// The visible lists are derived from the same object: every parsed
		// entry id renders exactly once, in inventory order.
		expect(attributionIds(body())).toEqual(parsed.entries.map((entry) => entry.id));
		expect(head()).toContain('https://');
	});

	it('renders an announced error instead of a partial list when inventory is invalid', async () => {
		vi.resetModules();
		vi.doMock('#lib/attributions/index.js', () => ({
			attributionInventory: { schemaVersion: 1, entries: [] },
			activeAttributions: [],
			plannedAttributions: [],
			fixtureAudioLicenceRecord: 'docs/fixture-audio-licences.md',
			validateAttributionInventory: (value: unknown) => {
				// Delegate to the real validator shape: empty entries are invalid.
				const entries = (value as { entries?: unknown[] })?.entries;
				return Array.isArray(entries) && entries.length > 0 ? [] : ['entries must be a non-empty array'];
			}
		}));
		const { default: BrokenSettings } = await import('./+page.svelte');
		const html = render(BrokenSettings).body;
		const about = html.slice(html.indexOf('id="about-nntune"'));
		expect(about).toContain('role="alert"');
		expect(about).toContain('Attribution data is unavailable.');
		expect(about).not.toContain('data-attribution-id=');
	});
});
