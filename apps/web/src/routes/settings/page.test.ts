import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';

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
			// Link names are unique per entry so link lists never repeat.
			expect(about, entry.id).toContain(`>Source — ${entry.name}</a>`);
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
		// Rendered as grouped lists (active first, then planned), never in a
		// different order than the inventory's own grouping.
		const ids = attributionIds(body());
		expect(ids).toEqual([
			...activeAttributions
				.filter((entry) => entry.kind !== 'data' && entry.kind !== 'artwork')
				.map((entry) => entry.id),
			...plannedAttributions.map((entry) => entry.id)
		]);
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
			plannedAttributions.every(
				(entry) => entry.status === 'planned' && (entry.kind === 'data' || entry.kind === 'artwork')
			)
		).toBe(true);
	});

	it('embeds the machine-readable inventory verbatim alongside the lists', () => {
		const html = body();
		const match = html.match(/id="nntune-attribution-inventory">([\s\S]*?)<\/script>/);
		expect(match, 'provenance script present').not.toBeNull();
		const payload = (match as RegExpMatchArray)[1] as string;
		expect(payload.startsWith('{"'), `payload prefix: ${JSON.stringify(payload.slice(0, 80))}`).toBe(
			true
		);
		// Verbatim means no HTML escaping: JSON quotes must not arrive as
		// &quot; (Svelte escapes interpolations inside <script> elements).
		expect(payload).not.toContain('&quot;');
		const parsed = JSON.parse(payload) as typeof attributionInventory;
		expect(validateAttributionInventory(parsed)).toEqual([]);
		expect(parsed).toEqual(attributionInventory);
		// The visible lists are derived from the same object: every parsed
		// entry still renders exactly once.
		expect(attributionIds(body()).sort()).toEqual(parsed.entries.map((entry) => entry.id).sort());
		expect(head()).toContain('nntune:attributions:schema');
	});
});
