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

// The visible page is the About section minus the machine-readable JSON
// provenance block, which stays in the page but is not user-visible. Absence
// assertions must run against this so inactive entries and compliance
// guidance that live only in the JSON do not count as rendered.
function visibleAbout(html: string): string {
	return aboutSection(html).replace(
		/<script type="application\/json" id="nntune-attribution-inventory">[\s\S]*?<\/script>/,
		''
	);
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
		const visible = visibleAbout(body());
		for (const entry of activeAttributions) {
			expect(about, entry.id).toContain(`data-attribution-id="${entry.id}" data-status="active"`);
			expect(about, entry.id).toContain(entry.name);
			expect(about, entry.id).toContain(entry.notice);
			expect(about, entry.id).toContain(`href="${entry.sourceUrl}"`);
			// Link names are unique per entry so link lists never repeat.
			expect(about, entry.id).toContain(`>Source — ${entry.name}</a>`);
			if (entry.id === 'figtree') {
				// The Figtree entry carries its source link only; the licence
				// link lives in the Typeface intro sentence (same-origin).
				expect(about).not.toContain(`href="${entry.licenseUrl}"`);
			} else {
				expect(about, entry.id).toContain(`href="${entry.licenseUrl}"`);
				expect(about, entry.id).toContain(`>MIT License — ${entry.name}</a>`);
			}
			// Entry obligations are internal compliance guidance: they stay in
			// the JSON provenance and never render to users.
			expect(visible, entry.id).not.toContain(entry.obligations);
		}
		// No "Planned sources" list and no compliance-dashboard text anywhere
		// on the visible page.
		expect(visible).not.toContain('Planned sources');
		expect(visible).not.toContain('Retain the upstream copyright');
		expect(visible).not.toContain('This summary does not replace the licence');
		// Original icon work, never a third-party icon-set credit.
		expect(about).not.toContain('Lucide');
		expect(about).toContain('NNTune uses original inline SVG stencils');
		// Approved licence intro lines from the design spec: "MIT License"
		// links the repository licence file (public repo) and "Open Font
		// License" links the shipped same-origin copy.
		expect(about).toContain(
			'NNTune is open source under the <a href="https://github.com/TogetherWeOwn/nntunev2/blob/main/LICENSE">MIT License</a>.'
		);
		expect(about).toContain(
			'Typeface: Figtree, SIL <a href="/fonts/OFL.txt">Open Font License</a> 1.1.'
		);
	});

	it('links the shipped Open Font License copy same-origin', () => {
		const about = aboutSection(body());
		expect(about).toContain('href="/fonts/OFL.txt"');
		// Exactly one shipped-copy link, in the Typeface intro sentence.
		expect([...about.matchAll(/href="\/fonts\/OFL\.txt"/g)]).toHaveLength(1);
		// The shipped copy is the only same-origin attribution link; every
		// other attribution href is an authoritative https URL.
		for (const href of [...about.matchAll(/<a href="([^"]+)"[^>]*>/g)].map((m) => m[1] as string)) {
			if (href === '/fonts/OFL.txt') continue;
			expect(href.startsWith('https://') || href.startsWith('/library') ||
				href.startsWith('/account'), href).toBe(true);
		}
	});

	it('omits planned sources from the visible page entirely, keeping them in provenance only', () => {
		const visible = visibleAbout(body());
		// The visible list is the active licence entries only, in inventory
		// order.
		expect(attributionIds(body())).toEqual(
			activeAttributions
				.filter((entry) => entry.kind !== 'data' && entry.kind !== 'artwork')
				.map((entry) => entry.id)
		);
		// No inactive source name, notice or id renders visibly: the accurate
		// inactive treatment is absence (no "coming soon" entries, no greyed
		// entries, no roadmap lists).
		for (const entry of plannedAttributions) {
			expect(visible, entry.id).not.toContain(`data-attribution-id="${entry.id}"`);
			expect(visible, entry.id).not.toContain(entry.name);
			expect(visible, entry.id).not.toContain(entry.notice);
		}
		// Raw unresolved-rights licence markers only ever existed on planned
		// entries; they must not appear on the visible page.
		expect(visible).not.toContain('third-party-rights');
		expect(visible).not.toContain('not-assessed');
		// No "Data sources" h3 while no data source is active.
		expect(visible).not.toContain('settings-about-sources-heading');
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
		// The JSON block keeps the full inventory, including planned entries
		// and obligations: the visible lists render only the active entries
		// derived from the same object.
		expect(parsed.entries.filter((entry) => entry.status === 'planned').map((entry) => entry.id))
			.toEqual(plannedAttributions.map((entry) => entry.id));
		expect(attributionIds(body()).sort()).toEqual(
			parsed.entries.filter((entry) => entry.status === 'active').map((entry) => entry.id).sort()
		);
		// Obligations stay machine-readable: present in the JSON, absent from
		// the visible page.
		for (const entry of parsed.entries) {
			expect(payload, entry.id).toContain(entry.obligations);
		}
		expect(visibleAbout(body())).not.toContain('Retain the upstream copyright');
		expect(head()).toContain('nntune:attributions:schema');
	});
});
