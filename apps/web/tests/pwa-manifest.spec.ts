import { expect, test } from '@playwright/test';

// Paired acceptance spec for the build card. The Builder builds on
// this branch and never edits this file. Contract pinned here:
// - GET /manifest.webmanifest serves application/manifest+json with name,
//   short_name, start_url, display: standalone, theme_color, and reachable
//   192px + 512px icons.
// - App head links the manifest and sets a matching theme-color meta.
// - Manifest description, home meta description, and the visible landing summary
//   describe today's demo, reject library/real-provider/offline promises,
//   and point to signup or BYOP setup. The full per-capability home contract is
//   protected separately in home-truthful-today.spec.ts.
// Theme color #0b0d12 matches --nt-bg (dark default) in
// src/lib/design/tokens.css. Name "NNTune" matches the landing heading.

const EXPECTED_MANIFEST = {
	name: 'NNTune',
	short_name: 'NNTune',
	start_url: '/',
	display: 'standalone',
	theme_color: '#0b0d12'
} as const;

const HONEST_TODAY =
	'Try the demo player and its queue without an account. Your account library, favorites and a library-backed queue are not available yet. ' +
	'Live search-to-stream over a real provider is not available. Streaming from a real provider is not available. ' +
	'Offline caching of audio is not available. Browser snapshots store metadata only. ' +
	'Bring your own Usenet provider account; access is never bundled or resold. Sign up to connect a provider.';

const UNAVAILABLE =
	/\b(?:not (?:yet )?(?:available|supported|working|wired|ready|implemented)|not wired(?: up)? yet|unavailable|unsupported|does not|doesn't|cannot|can't|no live|no offline)\b/i;
// The demo queue lives in this tab's sessionStorage: it does not sync, follow
// you to another device or browser, or persist beyond the session.
const DEMO_QUEUE_OVERREACH =
	/\b(?:sync\w*|(?:across|other|another|every|any|all)\s+(?:of\s+)?(?:your\s+)?(?:devices?|browsers?)|cloud|follows?\s+you|back(?:ed)?[- ]?up\w*|permanent\w*|forever|server\w*|offline)\b/i;
const PROVIDER_SCOPE = /\b(?:real|live|Usenet|providers?)\b|\bsearch[- ]to[- ]stream\b/i;
const UNAVAILABLE_CAPABILITIES = [
	['library', /\blibrar\w*\b/i],
	['queue', /\bqueue\w*\b/i],
	['search', /\bsearch\w*\b/i],
	['streaming', /\bstream\w*\b/i],
	['offline caching', /\boffline(?:[- ]friendly)? (?:audio )?cach\w*\b|\boffline (?:audio|music)\b|\b(?:listen|play|enjoy|take your music)\s+(?:anywhere|offline)\b/i]
] as const;

function expectHonestPublicCopy(copy: unknown, surface: string): void {
	expect(typeof copy, `${surface} should contain a description`).toBe('string');
	const text = (copy as string).replace(/\s+/g, ' ').trim();
	const sentences = text.split(/[.!?;]+/).filter((sentence) => sentence.trim());

	// Summaries may be brief; the full home bank pins each capability's detail.
	// The demo needs no unwired disclaimer.
	expect(
		sentences.some((sentence) => /\bdemo\b/i.test(sentence) && !UNAVAILABLE.test(sentence)),
		`${surface} should describe an available labelled demo`
	).toBe(true);

	// Keep disclosures sentence-local: honest copy elsewhere must not excuse an
	// affirmative library, real-provider or offline-audio promise. Explicit demo
	// streaming is not a real-provider chain, but the label cannot excuse one.
	// The web app has no /api/library route yet, so no library claim is exempt.
	// The demo player's own queue works; only a library-backed queue is unwired.
	// The demo queue is session-local, so a demo sync or cross-device claim is not exempt.
	for (const [capability, pattern] of UNAVAILABLE_CAPABILITIES) {
		for (const sentence of sentences.filter((sentence) => pattern.test(sentence))) {
			const demo = /\bdemo\b/i.test(sentence);
			if (capability === 'queue' && demo && !/\blibrar\w*\b/i.test(sentence) && !DEMO_QUEUE_OVERREACH.test(sentence)) continue;
			if ((capability === 'search' || capability === 'streaming') && demo && !PROVIDER_SCOPE.test(sentence)) continue;
			expect(sentence, `${surface}: ${capability} must be explicitly unavailable`).toMatch(UNAVAILABLE);
		}
	}

	expect(text, `${surface} should point to signup or bring-your-own-provider setup`).toMatch(
		/\b(?:sign[ -]?up|bring[- ]your[- ]own[- ]providers?|BYOP)\b/i
	);
}

test('accepts the truthful demo summary without a blanket disclaimer', async () => {
	expect(() => expectHonestPublicCopy(HONEST_TODAY, 'demo control')).not.toThrow();
});

test('accepts an unwired library beside an unwired provider chain', async () => {
	expect(() =>
		expectHonestPublicCopy(HONEST_TODAY.replaceAll('not available', 'not wired yet'), 'library control')
	).not.toThrow();
});

test('accepts a concise demo summary without listing every unavailable feature', async () => {
	expect(() =>
		expectHonestPublicCopy('Try the demo player. Sign up and bring your own provider.', 'concise summary control')
	).not.toThrow();
});

for (const claim of [
	'NNTune manages your library locally in your browser.',
	'NNTune manages your libraries locally in your browser.',
	'Try the demo player and browser library.',
	'Search your local library in your browser.'
]) {
	test(`rejects an affirmative library claim: ${claim}`, async () => {
		expect(() => expectHonestPublicCopy(`${claim} ${HONEST_TODAY}`, 'library mutant')).toThrow(
			'library must be explicitly unavailable'
		);
	});
}

for (const claim of [
	'Your queue follows you across devices.',
	'Queue tracks from your library.',
	'Try the demo player and your library queue.'
]) {
	test(`rejects an affirmative non-demo queue claim: ${claim}`, async () => {
		expect(() => expectHonestPublicCopy(`${claim} ${HONEST_TODAY}`, 'queue mutant')).toThrow(
			/(?:queue|library) must be explicitly unavailable/
		);
	});
}

test('accepts the working demo queue', async () => {
	expect(() => expectHonestPublicCopy(
		`The demo player has a working queue you can reorder. ${HONEST_TODAY}`,
		'demo queue control'
	)).not.toThrow();
});

for (const claim of [
	'The demo queue follows you across devices.',
	'The demo queue syncs to your other browsers.',
	'The demo player saves its queue to the cloud.',
	'The demo queue is backed up permanently.'
]) {
	test(`rejects an unsupported demo queue claim: ${claim}`, async () => {
		expect(() => expectHonestPublicCopy(`${claim} ${HONEST_TODAY}`, 'demo queue mutant')).toThrow(
			'queue must be explicitly unavailable'
		);
	});
}

test('accepts demo streaming', async () => {
	expect(() => expectHonestPublicCopy(
		`The demo player streams sample tracks. ${HONEST_TODAY}`,
		'demo streaming control'
	)).not.toThrow();
});

test('rejects the old streams headline prepended to otherwise honest copy', async () => {
	// Survivor regression: /\bstreaming\b/i missed the exact old headline.
	const mutant = `NNTune streams your music on Cloudflare. ${HONEST_TODAY}`;

	expect(() => expectHonestPublicCopy(HONEST_TODAY, 'regression control')).not.toThrow();
	expect(() => expectHonestPublicCopy(mutant, 'regression mutant')).toThrow(
		'streaming must be explicitly unavailable'
	);
});

// Exercise the real helper, not the regexes in isolation: an honest
// disclaimer must not excuse a separate affirmative sibling word form.
for (const [capability, wording, claim] of [
	['search', 'search', 'NNTune search finds music on Usenet.'],
	['search', 'searching', 'NNTune is searching Usenet for your music.'],
	['search', 'searches', 'NNTune searches Usenet for your music.'],
	['search', 'searched', 'NNTune searched Usenet for your music.'],
	['offline caching', 'offline cache', 'Your offline cache is ready anywhere.'],
	['offline caching', 'offline caches', 'Your offline caches are ready anywhere.'],
	['offline caching', 'offline cached', 'Your music is offline cached for travel.'],
	['offline caching', 'offline caching', 'Offline caching keeps your music ready for travel.'],
	['offline caching', 'offline-friendly caches', 'Your offline-friendly caches are ready anywhere.'],
	['offline caching', 'offline friendly cached', 'Your music is offline friendly cached for travel.']
] as const) {
	test(`rejects affirmative ${wording} beside otherwise honest copy`, async () => {
		expect(() => expectHonestPublicCopy(HONEST_TODAY, 'sibling control')).not.toThrow();
		expect(() => expectHonestPublicCopy(`${claim} ${HONEST_TODAY}`, 'sibling mutant')).toThrow(
			`${capability} must be explicitly unavailable`
		);
		expect(() => expectHonestPublicCopy(
			HONEST_TODAY.replace(new RegExp(capability, 'i'), wording),
			'sibling disclosure control'
		)).not.toThrow();
	});
}

for (const [capability, claim] of [
	['search', 'NNTune searches Usenet locally in your browser.'],
	['streaming', 'The demo player streams music from your real provider.'],
	['offline caching', 'Offline audio caching is ready now.'],
	['offline caching', 'Listen offline now.']
] as const) {
	test(`rejects a misleading capability claim: ${claim}`, async () => {
		expect(() => expectHonestPublicCopy(`${claim} ${HONEST_TODAY}`, 'capability mutant')).toThrow(
			`${capability} must be explicitly unavailable`
		);
	});
}

type ManifestIcon = {
	src?: unknown;
	sizes?: unknown;
	type?: unknown;
};

function maxIconDimension(sizes: unknown): number {
	if (typeof sizes !== 'string') return 0;
	let max = 0;
	for (const token of sizes.split(/\s+/)) {
		const match = /^(\d+)x(\d+)$/.exec(token.trim());
		if (!match) continue;
		max = Math.max(max, Number(match[1]), Number(match[2]));
	}
	return max;
}

test('/manifest.webmanifest serves a valid PWA manifest with reachable icons', async ({
	request
}) => {
	const response = await request.get('/manifest.webmanifest');

	expect(response.status(), '/manifest.webmanifest should exist (not 404)').toBe(200);
	expect(response.ok()).toBe(true);

	const contentType = response.headers()['content-type'] ?? '';
	expect(contentType).toContain('application/manifest+json');

	const manifest = (await response.json()) as Record<string, unknown>;

	expect(manifest.name, 'manifest should have a non-empty name').toBe(
		EXPECTED_MANIFEST.name
	);
	expect(manifest.short_name, 'manifest should have a non-empty short_name').toBe(
		EXPECTED_MANIFEST.short_name
	);
	expect(manifest.start_url, 'manifest start_url should be /').toBe(
		EXPECTED_MANIFEST.start_url
	);
	expect(manifest.display, 'manifest display should be standalone').toBe(
		EXPECTED_MANIFEST.display
	);
	expect(manifest.theme_color, 'manifest theme_color should match the design token').toBe(
		EXPECTED_MANIFEST.theme_color
	);

	const icons = manifest.icons as ManifestIcon[] | undefined;
	expect(Array.isArray(icons), 'manifest should have an icons array').toBe(true);
	expect(icons!.length, 'manifest should declare at least one icon').toBeGreaterThan(0);

	const maxSize = Math.max(...icons!.map((icon) => maxIconDimension(icon.sizes)));
	expect(maxSize, 'manifest should declare an icon of at least 192px').toBeGreaterThanOrEqual(
		192
	);
	expect(maxSize, 'manifest should declare an icon of at least 512px').toBeGreaterThanOrEqual(
		512
	);

	// Every declared icon src must resolve to a real 200 response. The
	// >=192px and >=512px entries are the installability gate, so fetch at
	// least one icon at each threshold explicitly.
	const iconSrcs = icons!
		.map((icon) => icon.src)
		.filter((src): src is string => typeof src === 'string' && src.length > 0);
	expect(iconSrcs.length, 'every icon should have a src').toBe(icons!.length);

	const large192 = icons!.find((icon) => maxIconDimension(icon.sizes) >= 192)?.src as string;
	const large512 = icons!.find((icon) => maxIconDimension(icon.sizes) >= 512)?.src as string;
	expect(large192, 'manifest should have an icon >=192px with a src').toBeTruthy();
	expect(large512, 'manifest should have an icon >=512px with a src').toBeTruthy();

	for (const src of new Set([large192, large512])) {
		const iconResponse = await request.get(src);
		expect(iconResponse.status(), `${src} should be served with 200`).toBe(200);
		expect(iconResponse.ok()).toBe(true);
		const iconContentType = iconResponse.headers()['content-type'] ?? '';
		expect(iconContentType, `${src} should be served as an image`).toMatch(/image\//);
	}
});

test('manifest description describes demo availability and points to setup', async ({ request }) => {
	const response = await request.get('/manifest.webmanifest');
	expect(response.ok()).toBe(true);
	const manifest = (await response.json()) as Record<string, unknown>;

	expectHonestPublicCopy(manifest.description, 'manifest description');
});

test('home meta description describes demo availability and points to setup', async ({ page }) => {
	await page.goto('/');
	const description = page.locator('meta[name="description"]');
	await expect(description).toHaveCount(1);

	expectHonestPublicCopy(await description.getAttribute('content'), 'home meta description');
});

test('home landing summary describes demo availability and points to setup', async ({ page }) => {
	await page.goto('/');
	const summary = await page.locator('meta[name="description"]').getAttribute('content');
	expectHonestPublicCopy(summary, 'home summary');
	const paragraph = page.getByRole('main').locator('p').filter({ hasText: summary! }).first();
	await expect(paragraph).toBeVisible();

	expectHonestPublicCopy(await paragraph.innerText(), 'home landing summary');
});

test('app head links the manifest and sets the matching theme-color', async ({ page }) => {
	// Layout-level contract: both the landing page and a second route must
	// carry the same head entries, proving they come from the app shell
	// (app.html / root layout), not a single page.
	for (const route of ['/', '/login']) {
		await page.goto(route);

		const manifestLink = page.locator('link[rel="manifest"]');
		await expect(manifestLink, `${route} should link the web manifest`).toHaveCount(1);
		await expect(manifestLink, `${route} manifest link should point at /manifest.webmanifest`).toHaveAttribute(
			'href',
			'/manifest.webmanifest'
		);

		const themeColor = page.locator('meta[name="theme-color"]');
		await expect(themeColor, `${route} should set a theme-color meta`).toHaveCount(1);
		await expect(themeColor, `${route} theme-color should match the manifest`).toHaveAttribute(
			'content',
			EXPECTED_MANIFEST.theme_color
		);
	}
});
