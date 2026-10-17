import { expect, test } from '@playwright/test';

// Paired acceptance spec for TOG-28448 (build card). The Builder builds on
// this branch and never edits this file. Contract pinned here:
// - GET /manifest.webmanifest serves application/manifest+json with name,
//   short_name, start_url, display: standalone, theme_color, and reachable
//   192px + 512px icons.
// - App head links the manifest and sets a matching theme-color meta.
// Theme color #0b0d12 matches --nt-bg (dark default) in
// src/lib/design/tokens.css. Name "NNTune" matches the landing heading.

const EXPECTED_MANIFEST = {
	name: 'NNTune',
	short_name: 'NNTune',
	start_url: '/',
	display: 'standalone',
	theme_color: '#0b0d12'
} as const;

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
