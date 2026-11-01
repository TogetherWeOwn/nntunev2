import { expect, test } from '@playwright/test';

// Protected acceptance bank: the Builder builds on this branch and never edits
// this file. The public home describes today's demo capabilities, not a
// working provider search-to-stream chain. No provider credentials are used.

const STALE_BLANKET =
	/search,?\s+streaming,?\s+library management\s+and\s+offline caching\s+are not wired yet/i;
const UNAVAILABLE =
	/\b(?:not (?:yet )?(?:available|supported|working|wired|ready|implemented)|not wired(?: up)? yet|unavailable|unsupported|does not|doesn't|cannot|can't|no live|no offline)\b/i;
const normalize = (text: string): string => text.replace(/\s+/g, ' ').trim();

for (const [capability, pattern] of [
	['bring your own Usenet provider', /bring[- ]your[- ]own\s+(?:Usenet\s+)?provider/i],
	['provider access is never bundled', /\b(?:never|not|does not|doesn't|no)\b[^.!?]*\bbundl\w*\b/i],
	['provider access is never resold', /\b(?:never|not|does not|doesn't|no)\b[^.!?]*\b(?:resell\w*|resold)\b/i],
	['an explicitly labelled demo player', /\bdemo\s+player\b/i],
	['demo does not require an account', /\b(?:without (?:an? )?account|no account (?:is )?(?:needed|required)|no sign[- ]?up (?:is )?(?:needed|required))\b/i],
	['create an account', /\b(?:create|make)\s+(?:an?\s+)?account\b|\bsign[ -]?up\b/i],
	['connect a provider', /\b(?:connect|add|configure|set up)\s+(?:your\s+(?:own\s+)?|an?\s+)?(?:Usenet\s+)?provider\b/i]
] as const) {
	test(`home today: explains ${capability}`, async ({ page }) => {
		await page.goto('/');
		const copy = normalize(await page.getByRole('main').innerText());
		expect(copy).toMatch(pattern);
		if (capability === 'bring your own Usenet provider') expect(copy).toMatch(/\bUsenet\b/i);
	});
}

test('home today: library, favorites and a library-backed queue are disclosed as not available yet', async ({ page }) => {
	// The web app has no /api/library route yet, so /library cannot list
	// anything; the home must not claim a working library, favorites or
	// library-backed queue. The demo player's own queue works and may be named.
	await page.goto('/');
	const blocks = (await page.getByRole('main').locator('p, li').allInnerTexts())
		.map(normalize).flatMap((text) => text.split(/[.!?;]+/));
	const demoQueue = (text: string): boolean =>
		/\bdemo\b/i.test(text) && /\bqueue\b/i.test(text) && !/\blibrar\w*\b/i.test(text);
	for (const capability of [/\blibrary\b/i, /\bfavou?rites\b/i, /\bqueue\b/i]) {
		const mentions = blocks.filter((text) => capability.test(text) && !demoQueue(text));
		expect(mentions.length, `${capability}: missing availability disclosure`).toBeGreaterThan(0);
		for (const text of mentions) expect(text, `${capability}: must be explicitly unavailable`).toMatch(UNAVAILABLE);
	}
	// The working demo queue must not be swept into the unavailable disclosure.
	const demo = blocks.filter(demoQueue);
	expect(demo.length, 'describe the working demo queue').toBeGreaterThan(0);
	for (const text of demo) expect(text, 'the demo queue works').not.toMatch(UNAVAILABLE);
});

test('home today: live provider search-to-stream is explicitly not available', async ({ page }) => {
	await page.goto('/');
	const copy = normalize(await page.getByRole('main').innerText());
	const sentences = copy.split(/[.!?;]+/);
	const live = sentences.filter((text) =>
		/\blive\s+search|\bsearch[- ]to[- ]stream|\breal\s+(?:Usenet\s+)?provider[^.!?]*\bstream/i.test(text)
	);
	expect(live.length, 'explicitly distinguish the live provider chain from the demo').toBeGreaterThan(0);
	for (const sentence of live) expect(sentence).toMatch(UNAVAILABLE);
	expect(copy).not.toMatch(/\b(?:search|stream)\s+(?:real\s+)?(?:Usenet\s+)?music\s+(?:now|today|instantly)\b/i);
});

test('home today: offline audio is unavailable and the browser snapshot is metadata only', async ({ page }) => {
	await page.goto('/');
	const copy = normalize(await page.getByRole('main').innerText());
	const offline = copy.split(/[.!?;]+/).filter((text) => /\boffline\b/i.test(text));
	expect(offline.length, 'state the offline audio limit explicitly').toBeGreaterThan(0);
	for (const sentence of offline) {
		if (/\b(?:audio|music|cach\w*)\b/i.test(sentence)) expect(sentence).toMatch(UNAVAILABLE);
	}
	expect(offline.some((text) => /\b(?:audio|music|cach\w*)\b/i.test(text) && UNAVAILABLE.test(text))).toBe(true);
	expect(copy).toMatch(/\bmetadata[- ]only\b|\bonly\s+metadata\b/i);
	for (const sentence of copy.split(/[.!?;]+/)) {
		if (/\b(?:listen|play|enjoy|take your music)\s+(?:anywhere|offline)\b/i.test(sentence)) {
			expect(sentence).toMatch(UNAVAILABLE);
		}
	}
});

test('home today: visible summary, meta and install manifest tell the same truthful story', async ({ page, request }) => {
	await page.goto('/');
	const meta = page.locator('meta[name="description"]');
	await expect(meta).toHaveCount(1);
	const summary = normalize((await meta.getAttribute('content')) ?? '');
	expect(summary.length).toBeGreaterThan(0);
	expect(summary).toMatch(/\bdemo\b/i);
	const library = summary.split(/[.!?;]+/).filter((text) => /\blibrary\b/i.test(text));
	expect(library.length).toBeGreaterThan(0);
	for (const text of library) expect(text).toMatch(UNAVAILABLE);
	expect(summary).not.toMatch(STALE_BLANKET);

	const response = await request.get('/manifest.webmanifest');
	expect(response.ok()).toBe(true);
	expect(response.headers()['content-type']).toMatch(/application\/manifest\+json/i);
	const manifest = (await response.json()) as { description?: unknown };
	expect(typeof manifest.description).toBe('string');
	expect(normalize(manifest.description as string)).toBe(summary);
	const paragraphs = (await page.getByRole('main').locator('p').allInnerTexts()).map(normalize);
	expect(paragraphs, 'the shared description must also be a visible home summary').toContain(summary);
	expect(normalize(await page.getByRole('main').innerText())).not.toMatch(STALE_BLANKET);
});

test('home today: no fabricated social proof, prices or affirmative provider/offline promises on any surface', async ({ page, request }) => {
	await page.goto('/');
	const response = await request.get('/manifest.webmanifest');
	expect(response.ok()).toBe(true);
	const manifest = (await response.json()) as { description?: string };
	const surfaces = [
		normalize(await page.getByRole('main').innerText()),
		normalize((await page.locator('meta[name="description"]').getAttribute('content')) ?? ''),
		normalize(manifest.description ?? '')
	];
	for (const text of surfaces) {
		expect(text).not.toMatch(STALE_BLANKET);
		expect(text).not.toMatch(/[$€£]\s*\d|\b\d+(?:\.\d+)?\s*(?:USD|EUR|GBP|per month|\/mo)\b/i);
		expect(text).not.toMatch(/\b(?:\d[\d,.]*\+?|thousands|millions)\s+(?:happy\s+)?(?:users|listeners|customers|subscribers|tracks|songs)\b/i);
		expect(text).not.toMatch(/\b(?:trusted by|testimonial|rated\s+\d|five[- ]star|5[- ]star)\b/i);
		expect(text).not.toMatch(/\b(?:NNTune\s+(?:streams?|searches?)\s+(?:your\s+)?(?:music|Usenet)|offline cach\w*\s+(?:keeps?|makes?|lets?|is ready))\b/i);
	}
	await expect(page.getByRole('main').locator('blockquote, [itemtype$="/Review"], [itemtype$="/AggregateRating"]')).toHaveCount(0);
});

for (const [label, href, heading] of [
	[/demo/i, '/demo/player', /demo/i],
	[/get started|create (?:an? )?account|sign[ -]?up/i, '/signup', /get started|sign[ -]?up/i],
	[/plans|pricing/i, '/pricing', /plans|pricing/i]
] as const) {
	test(`home today: descriptive ${href} anchor opens the real destination`, async ({ page }) => {
		await page.goto('/');
		const link = page.getByRole('main').getByRole('link', { name: label });
		const destination = link.and(page.locator(`a[href="${href}"]`)).first();
		await expect(destination).toBeVisible();
		await expect(destination).toHaveAttribute('href', href);
		expect(await destination.getAttribute('target')).not.toBe('_blank');
		const response = await page.request.get(href);
		expect(response.ok(), `${href} must resolve, not just look like a link`).toBe(true);
		await destination.click();
		await expect(page).toHaveURL(new RegExp(`${href}$`));
		await expect(page.getByRole('heading', { level: 1 })).toContainText(heading);
	});
}

test('home today: existing sign-in recovery still opens the sign-in form', async ({ page }) => {
	await page.route('**/api/library*', (route) => route.fulfill({
		status: 401,
		contentType: 'application/json',
		body: JSON.stringify({ error: 'unauthorized' })
	}));
	await page.goto('/library');
	const signIn = page.getByRole('link', { name: 'Sign in', exact: true });
	await expect(signIn).toHaveAttribute('href', '/login');
	await signIn.click();
	await expect(page).toHaveURL(/\/login$/);
	await expect(page.getByLabel('Email', { exact: true })).toBeVisible();
});

test('home today: one heading, intact landmarks and keyboard skip navigation', async ({ page }) => {
	await page.goto('/');
	await expect(page.getByTestId('app-root')).toBeVisible();
	await expect(page.getByRole('heading', { level: 1 })).toHaveCount(1);
	await expect(page.getByRole('heading', { level: 1 })).toContainText('NNTune');
	await expect(page.getByRole('main')).toHaveCount(1);
	await expect(page.getByRole('main').locator('main')).toHaveCount(0);
	await expect(page.locator('nav:has(a[href="/library/import"])')).toHaveCount(1);
	await expect(page.getByRole('navigation', { name: 'Primary', exact: true })).toHaveCount(1);
	await page.keyboard.press('Tab');
	const skip = page.getByRole('link', { name: /skip to (?:main )?content/i });
	await expect(skip).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(page.getByRole('main')).toBeFocused();
});

test('home today: no external assets, tracking or mutating requests', async ({ page, baseURL }, testInfo) => {
	const origin = new URL(baseURL!).origin;
	const requests: { url: string; method: string }[] = [];
	await page.route('**/*', async (route) => {
		const request = route.request();
		requests.push({ url: request.url(), method: request.method() });
		if (new URL(request.url()).origin !== origin) return route.abort();
		return route.continue();
	});
	try {
		await page.goto('/');
		await expect(page.getByTestId('app-root')).toBeVisible();
		const declaredAssets = await page.locator(
			'script[src], link[href][rel~="stylesheet"], link[href][rel~="preload"], link[href][rel~="modulepreload"], link[href][rel~="icon"], link[href][rel~="manifest"], img[src], source[src], video[poster], iframe[src], object[data]'
		).evaluateAll((nodes) => nodes.map((node) =>
			node.getAttribute('src') ?? node.getAttribute('href') ?? node.getAttribute('poster') ?? node.getAttribute('data') ?? ''
		));
		for (const asset of declaredAssets) {
			// Inline data:/blob: assets (e.g. the embedded SVG icon) are not
			// external assets; their URL origin is the string "null".
			if (/^(?:data|blob):/i.test(asset)) continue;
			expect(new URL(asset, page.url()).origin, asset).toBe(origin);
		}
		for (const request of requests) {
			expect(new URL(request.url).origin, request.url).toBe(origin);
			expect(['GET', 'HEAD'], request.url).toContain(request.method);
			expect(new URL(request.url).pathname).not.toMatch(/\b(?:analytics|telemetry|tracking|collect)\b/i);
		}
	} finally {
		await testInfo.attach('home-requests', { body: JSON.stringify(requests, null, 2), contentType: 'application/json' });
	}
});
