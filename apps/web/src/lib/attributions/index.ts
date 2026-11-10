import type { Attribution, AttributionInventory } from './contract.js';
export { validateAttributionInventory } from './contract.js';
export type { Attribution, AttributionInventory } from './contract.js';

function runtimePackage(
	name: string, specifier: string, version: string, sourceUrl: string, licenseUrl: string
): Attribution {
	return {
		id: name, name, kind: 'package', status: 'active', license: 'MIT',
		sourceUrl, licenseUrl,
		notice: `NNTune uses ${name}, distributed under the MIT licence.`,
		obligations: 'Retain the upstream copyright and full MIT permission notice in distributed copies or substantial portions. This summary does not replace the licence.',
		evidence: ['apps/web/package.json', 'apps/web/pnpm-lock.yaml', 'apps/web/vite.config.ts'],
		package: { name, specifier, version }
	};
}

/** Direct web build/test tools: classified, but not claimed as shipped runtime packages. */
export const WEB_TOOL_PACKAGES = [
	'@axe-core/playwright', '@playwright/test', '@sveltejs/vite-plugin-svelte',
	'svelte-check', 'terser', 'typescript', 'vite', 'vitest'
] as const;

/** First-party, JSON-serializable inventory; see docs/attributions.md for scope and upkeep. */
export const attributionInventory: AttributionInventory = {
	schemaVersion: 1,
	entries: [
		runtimePackage('svelte', '^5.57.3', '5.57.3',
			'https://github.com/sveltejs/svelte', 'https://github.com/sveltejs/svelte/blob/main/LICENSE.md'),
		runtimePackage('@sveltejs/kit', '^3.1.1', '3.1.1',
			'https://github.com/sveltejs/kit', 'https://github.com/sveltejs/kit/blob/main/LICENSE'),
		runtimePackage('@sveltejs/adapter-cloudflare', '^8.0.2', '8.0.2',
			'https://github.com/sveltejs/kit/tree/main/packages/adapter-cloudflare',
			'https://github.com/sveltejs/kit/blob/main/LICENSE'),
		{
			id: 'figtree', name: 'Figtree', kind: 'font', status: 'active', license: 'OFL-1.1',
			sourceUrl: 'https://github.com/google/fonts/tree/main/ofl/figtree',
			licenseUrl: 'https://github.com/google/fonts/blob/main/ofl/figtree/OFL.txt',
			notice: 'Figtree: Copyright 2022 The Figtree Project Authors. SIL Open Font License 1.1.',
			obligations: 'Keep the full copyright and OFL notice with the self-hosted font files; observe the OFL conditions when modifying or redistributing them. System-ui is a platform fallback, not a bundled font.',
			evidence: ['apps/web/static/fonts/FONTS.md', 'apps/web/static/fonts/OFL.txt', 'apps/web/src/lib/design/tokens.css']
		},
		{
			id: 'nntune-icons', name: 'NNTune icon stencils', kind: 'icons', status: 'active', license: 'MIT',
			sourceUrl: 'https://github.com/TogetherWeOwn/nntunev2/blob/main/apps/web/src/lib/design/icons.ts',
			licenseUrl: 'https://github.com/TogetherWeOwn/nntunev2/blob/main/LICENSE',
			notice: 'NNTune uses original inline SVG stencils drawn for this project.',
			obligations: 'Covered by the repository MIT licence; retain its copyright and permission notice. No third-party icon package is installed.',
			evidence: ['apps/web/src/lib/design/icons.ts', 'apps/web/src/lib/design/README.md', 'LICENSE']
		},
		{
			id: 'musicbrainz-core', name: 'MusicBrainz core data', kind: 'data', status: 'planned', license: 'CC0-1.0',
			sourceUrl: 'https://musicbrainz.org/doc/MusicBrainz_Database',
			licenseUrl: 'https://musicbrainz.org/doc/About/Data_License',
			notice: 'MusicBrainz core catalogue data is planned, not yet consumed by product reads; the replacement mirror has not completed import or activated reads.',
			obligations: 'Inactive. CC0 does not require attribution. Core data only, including URL relationships; supplementary tags, genres, ratings, annotations and hourly Live Data Feed packets are not covered by this entry and must not enter the paid product mirror. Recheck the consumed dataset and mirror activation before activation; serve product reads from the mirror.',
			evidence: ['workers/mb/src/lib.rs', 'docs/adr/0007-mb-mirror.md', 'AGENTS.md']
		},
		{
			id: 'cover-art', name: 'Referenced cover art', kind: 'artwork', status: 'planned', license: 'third-party-rights',
			sourceUrl: 'https://coverartarchive.org/',
			licenseUrl: 'https://musicbrainz.org/doc/Cover_Art_Archive',
			notice: 'Cover art has separate third-party rights; catalogue availability is not a redistribution licence. Not yet consumed; the catalogue mirror has not activated reads.',
			obligations: 'Inactive. Rights-flagged references only in this inventory, not a licence grant. No new caching, copying or redistribution is authorized; review rights and any new cache separately.',
			evidence: ['workers/mb/src/lib.rs', 'apps/web/src/lib/player/artwork.ts', 'AGENTS.md']
		},
		{
			id: 'listenbrainz', name: 'ListenBrainz dumps', kind: 'data', status: 'planned', license: 'CC0-1.0',
			sourceUrl: 'https://listenbrainz.org/data/', licenseUrl: 'https://listenbrainz.org/data/',
			notice: 'ListenBrainz dumps are planned, not integrated or consumed by this feature.',
			obligations: 'Inactive. Recheck the integrated dataset and its CC0 terms before activation; this entry does not authorize integration.',
			evidence: ['AGENTS.md', 'docs/attributions.md']
		},
		{
			id: 'lrclib', name: 'LRCLIB', kind: 'data', status: 'planned', license: 'not-assessed',
			sourceUrl: 'https://lrclib.net/', licenseUrl: 'https://lrclib.net/docs',
			notice: 'LRCLIB is planned, not integrated. No lyrics licence is asserted.',
			obligations: 'Inactive. Service documentation is a provenance pointer, not a content licence. Assess lyrics rights and applicable terms before any integration; a server software licence does not clear lyrics rights.',
			evidence: ['docs/attributions.md']
		}
	]
};

/** Future UI consumers must not present planned integrations as current sources. */
export const activeAttributions = attributionInventory.entries.filter((entry) => entry.status === 'active');
export const plannedAttributions = attributionInventory.entries.filter((entry) => entry.status === 'planned');
export const fixtureAudioLicenceRecord = 'docs/fixture-audio-licences.md';
