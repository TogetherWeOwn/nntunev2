import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * App-shell navigation acceptance (roadmap item NAV): reach Library, local NZB
 * selection and Account from a compact shell landmark without typing URLs.
 *
 * Protected Test Author handoff: the Builder continues on this branch and
 * never edits this file. Every failing `expect` below is the build contract;
 * change production code (the app shell), never these pins.
 *
 * Stable case IDs:
 *   NAV-01  shell landmark with Library / Select NZB locally / Account links
 *   NAV-02a direct load marks Library vs Select NZB locally (most specific)
 *   NAV-02b direct load marks Account on nested account pages
 *   NAV-02c direct load marks nothing outside the three destinations
 *   NAV-02d direct load matches nested paths by pathname boundary
 *   NAV-03  client-side navigation updates the marker without a reload
 *   NAV-04  skip link reveals on focus and moves keyboard focus to the page main (wide + narrow)
 *   NAV-05  keyboard order is skip, nav links, Appearance, content (wide + narrow)
 *   NAV-06  visible focus ring on shell links (wide + narrow)
 *   NAV-07 390px/360px narrow widths wrap, no overflow, no hamburger
 *   NAV-08a links stay discoverable with a populated library
 *   NAV-08b links stay discoverable with an empty library
 *   NAV-08c links stay discoverable with an unavailable library
 *   NAV-08d links stay discoverable with a cached (stale) library
 *   NAV-09  navigating emits no mutating or off-origin requests
 *   NAV-10  demo player links carry explicit demo labeling, no playback claims
 *   NAV-11  shell navigation preserves the local-only import disclosure
 *   NAV-12  account subnav keeps working alongside the shell nav
 *   NAV-13  paywall close keeps focus restoration and modal isolation (Tab + Shift+Tab) with nav
 *   NAV-14  paywall upgrade navigation keeps the shell and a sane focus target
 *   NAV-15  existing player shortcuts keep working with the shell present
 *   NAV-16  PWA head, landing honesty and login stay intact across navigation
 *   NAV-17  navigation adds no authenticated state and keeps sign-in recovery
 *   NAV-18  native links plus design-token styling, no new route
 *
 * RED expectations: with no shell nav on main, every test fails on an
 * `expect` naming the missing UI (assertion-RED). No test imports project
 * code, so a failure is never an import/compile error. Each message states
 * the expected UI and the Builder's next action.
 *
 * Run focused (from the repo root):
 *   pnpm --dir apps/web test:e2e app-shell-navigation
 * Typecheck the app (specs are browser-run, not part of `check`):
 *   pnpm --dir apps/web check
 */

const LIBRARY_ITEMS = [
	{ id: 'nav-track-1', title: 'Navigation Song One', artist: 'Shell Band' },
	{ id: 'nav-track-2', title: 'Navigation Song Two', artist: 'Shell Band' }
];

const PRESENCE_TIMEOUT = 3000;
const BUILDER = 'Builder: add the compact shell nav landmark to the root layout.';

/** The shell nav, located by behavior (it owns the /library/import link), not by name. */
function shellNav(page: Page): Locator {
	return page.locator('nav:has(a[href="/library/import"])');
}

function libraryLink(page: Page): Locator {
	return shellNav(page).getByRole('link', { name: 'Library', exact: true });
}

function importLink(page: Page): Locator {
	return shellNav(page).getByRole('link', { name: 'Select NZB locally', exact: true });
}

function accountLink(page: Page): Locator {
	return shellNav(page).getByRole('link', { name: 'Account', exact: true });
}

function skipLink(page: Page): Locator {
	return page.getByRole('link', { name: /skip to (main )?content/i });
}

function currentMarkers(page: Page): Locator {
	return shellNav(page).locator('[aria-current="page"]');
}

async function mockLibrary(page: Page, status: number, body: unknown): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })
	);
}

/** Account pages without backend or paywall interference (no 402/429 here). */
async function mockAccountApis(page: Page): Promise<void> {
	await page.route('**/v1/vault', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({
				provider_configured: false,
				indexer_configured: false,
				updated_at: 0
			})
		})
	);
	await page.route('**/v1/me/*', (route) =>
		route.fulfill({ status: 503, contentType: 'application/json', body: '{}' })
	);
}

async function expectShellPresent(page: Page, where: string): Promise<void> {
	await expect(
		shellNav(page),
		`expected one app-shell nav owning the /library/import link on ${where}; ` +
			`got none. ${BUILDER}`
	).toHaveCount(1, { timeout: PRESENCE_TIMEOUT });
}

async function expectCurrent(
	page: Page,
	path: string,
	expected: 'Library' | 'Select NZB locally' | 'Account' | null
): Promise<void> {
	const nav = shellNav(page);
	if (expected === null) {
		await expect(
			currentMarkers(page),
			`expected no aria-current marker in the shell nav on ${path}; ` +
				`a marker leaked onto a non-destination. ` +
				'Builder: match destinations by pathname boundary only.'
		).toHaveCount(0, { timeout: PRESENCE_TIMEOUT });
		return;
	}
	const link =
		expected === 'Library'
			? libraryLink(page)
			: expected === 'Select NZB locally'
				? importLink(page)
				: accountLink(page);
	await expect(
		link,
		`expected the ${expected} link (and only it) marked aria-current="page" on ${path}. ${BUILDER}`
	).toHaveAttribute('aria-current', 'page', { timeout: PRESENCE_TIMEOUT });
	await expect(
		currentMarkers(page),
		`expected exactly one aria-current marker in the shell nav on ${path}; ` +
			`both Library and Select NZB locally (or another pair) lit up. ` +
			'Builder: mark only the most-specific boundary match.'
	).toHaveCount(1, { timeout: PRESENCE_TIMEOUT });
	// The account subnav is a separate landmark: the shell must not steal its marker.
	if (path.startsWith('/account/')) {
		const subnav = page.getByRole('navigation', { name: 'Account', exact: true });
		await expect(
			subnav.locator('[aria-current="page"]'),
			`expected the account subnav to keep its own current marker on ${path}. ` +
				'Builder: leave the account subnav untouched.'
		).toHaveCount(1, { timeout: PRESENCE_TIMEOUT });
	}
	void nav;
}

/**
 * The focused skip link must be visibly revealed, not just focused:
 * `toBeVisible` passes for an off-viewport transform, so assert the rendered
 * box intersects the viewport. Pins the `:focus-visible` reveal in the root
 * layout (deleting it leaves the link translated above the viewport).
 */
async function expectSkipLinkRevealed(page: Page, where: string): Promise<void> {
	const box = await skipLink(page).boundingBox();
	expect(
		box,
		`expected the focused skip link on ${where} to have a rendered box. ` +
			'Builder: keep the skip link rendered (off-viewport when idle), not display:none.'
	).not.toBeNull();
	if (!box) return;
	const viewportHeight = page.viewportSize()?.height ?? 800;
	expect(
		box.y + box.height,
		`expected the focused skip link on ${where} to be revealed inside the viewport, ` +
			`got box y=${box.y} height=${box.height} (bottom ${box.y + box.height}). ` +
			'Builder: keep the .skip-link:focus-visible reveal that cancels the idle translate.'
	).toBeGreaterThan(0);
	expect(
		box.y,
		`expected the focused skip link on ${where} to sit below the viewport top, got y=${box.y}. ` +
			'Builder: keep the .skip-link:focus-visible reveal at the top of the viewport.'
	).toBeLessThan(viewportHeight);
}

/** Live focus state over the paywall dialog's focusable controls (links + enabled buttons). */
async function paywallFocusState(page: Page): Promise<{
	count: number;
	inside: boolean;
	onFirst: boolean;
	onLast: boolean;
}> {
	return page.evaluate(() => {
		const open = document.querySelector('[role="dialog"]');
		const controls = [
			...(open?.querySelectorAll<HTMLElement>('a[href], button:not(:disabled)') ?? [])
		].filter((control) => control.getClientRects().length > 0);
		return {
			count: controls.length,
			inside: !!open?.contains(document.activeElement),
			onFirst: controls.length > 0 && document.activeElement === controls[0],
			onLast: controls.length > 0 && document.activeElement === controls[controls.length - 1]
		};
	});
}

test('[NAV-01] shell exposes a named landmark with Library, Select NZB locally and Account', async ({
	page
}) => {
	await mockLibrary(page, 200, []);
	await page.goto('/library');
	await expectShellPresent(page, '/library');

	const nav = shellNav(page);
	await expect(
		nav,
		`expected the shell nav to carry a non-empty accessible name so landmarks stay distinguishable. ${BUILDER}`
	).toHaveAccessibleName(/.+/);
	await expect(
		nav,
		'expected the shell nav name to differ from the account subnav ("Account"). ' +
			'Builder: name the shell landmark Primary, Main, App or similar.'
	).not.toHaveAccessibleName('Account');

	for (const [label, link] of [
		['Library', libraryLink(page)],
		['Select NZB locally', importLink(page)],
		['Account', accountLink(page)]
	] as const) {
		await expect(
			link,
			`expected a visible shell link named exactly "${label}". ${BUILDER}`
		).toBeVisible({ timeout: PRESENCE_TIMEOUT });
		const tag = await link.evaluate((element) => element.tagName);
		expect(
			tag,
			`expected the "${label}" shell control to be a native link, got <${tag}>. ` +
				'Builder: use plain <a href>, never a button or div.'
		).toBe('A');
	}
	await expect(libraryLink(page)).toHaveAttribute('href', '/library');
	await expect(importLink(page)).toHaveAttribute('href', '/library/import');
	await expect(accountLink(page)).toHaveAttribute('href', /^\/account(\/|$)/);

	// No hamburger, modal menu or nav-local buttons: every destination link is
	// visible without opening anything.
	await expect(
		nav.getByRole('button'),
		'expected zero buttons inside the shell nav (no hamburger or menu toggle). ' +
			'Builder: render every destination as a wrapping link row.'
	).toHaveCount(0);
	await expect(
		page.getByRole('dialog'),
		'expected no dialog on a plain library load (no modal menu). ' +
			'Builder: keep navigation a link row, not a modal.'
	).toHaveCount(0);
});

test('[NAV-02a] direct load marks Library vs Select NZB locally, never both', async ({
	page
}) => {
	await mockLibrary(page, 200, []);
	await page.goto('/library');
	await expectShellPresent(page, '/library');
	await expectCurrent(page, '/library', 'Library');

	await page.goto('/library/import');
	await expectCurrent(page, '/library/import', 'Select NZB locally');
	await expect(
		libraryLink(page),
		'expected the Library link NOT marked current on /library/import. ' +
			'Builder: /library/import must win as the most-specific match.'
	).not.toHaveAttribute('aria-current', 'page');
});

test('[NAV-02b] direct load marks Account on nested account pages', async ({ page }) => {
	await mockAccountApis(page);
	for (const path of ['/account/billing', '/account/security', '/account/provider', '/account']) {
		await page.goto(path);
		await expectShellPresent(page, path);
		await expectCurrent(page, path, 'Account');
	}
});

test('[NAV-02c] direct load marks nothing outside the three destinations', async ({
	page
}) => {
	await mockLibrary(page, 200, []);
	await mockAccountApis(page);
	for (const path of ['/', '/login', '/demo/player', '/library2', '/accountant']) {
		await page.goto(path);
		await expectShellPresent(page, path);
		await expectCurrent(page, path, null);
	}
});

test('[NAV-02d] direct load matches nested paths by pathname boundary', async ({ page }) => {
	await mockLibrary(page, 200, []);
	await page.goto('/library/import/extra');
	await expectShellPresent(page, '/library/import/extra');
	await expectCurrent(page, '/library/import/extra', 'Select NZB locally');

	await page.goto('/library/deep-link');
	await expectCurrent(page, '/library/deep-link', 'Library');
});

test('[NAV-03] client-side navigation updates the marker without a reload', async ({ page }) => {
	await mockLibrary(page, 200, []);
	await mockAccountApis(page);
	await page.goto('/');
	await expectShellPresent(page, '/');
	await expectCurrent(page, '/', null);
	await page.evaluate(() => {
		(window as unknown as { __shellNavMarker: string }).__shellNavMarker = 'still-here';
	});

	await libraryLink(page).click();
	await expect(page).toHaveURL(/\/library$/);
	await expectCurrent(page, '/library (client nav)', 'Library');

	await importLink(page).click();
	await expect(page).toHaveURL(/\/library\/import$/);
	await expectCurrent(page, '/library/import (client nav)', 'Select NZB locally');

	await accountLink(page).click();
	await expect(page).toHaveURL(/\/account/);
	await expect(
		page.getByRole('navigation', { name: 'Account', exact: true }),
		'expected the Account shell link to land on a real account page showing the account subnav. ' +
			'Builder: point Account at an existing /account/* route (bare /account has no page).'
	).toBeVisible();
	await expectCurrent(page, 'account landing (client nav)', 'Account');

	const marker = await page.evaluate(
		() => (window as unknown as { __shellNavMarker?: string }).__shellNavMarker
	);
	expect(
		marker,
		'expected shell-link clicks to navigate client-side (window state survived); ' +
			'got a full reload. Builder: use native SvelteKit <a href> links.'
	).toBe('still-here');
	await expect(
		page.locator('main'),
		'expected exactly one main landmark after client-side navigation. ' +
			'Builder: do not add nested <main> elements.'
	).toHaveCount(1);
});

for (const width of [1280, 390]) {
	test(`[NAV-04 ${width}px] skip link moves keyboard focus to the page main`, async ({
		page
	}) => {
		await page.setViewportSize({ width, height: 800 });
		await mockLibrary(page, 200, []);
		await mockAccountApis(page);
		for (const path of ['/library', '/library/import', '/account/billing']) {
			await page.goto(path);
			await expect(
				skipLink(page),
				`expected a "Skip to content" link on ${path} at ${width}px. ${BUILDER}`
			).toHaveCount(1, { timeout: PRESENCE_TIMEOUT });
			await expect(skipLink(page)).toHaveAttribute('href', /^#/);

			await page.keyboard.press('Tab');
			await expect(
				skipLink(page),
				`expected the first Tab stop on ${path} at ${width}px to be the skip link. ` +
					'Builder: put the skip link first in DOM order.'
			).toBeFocused();
			await expectSkipLinkRevealed(page, `${path} at ${width}px`);

			await page.keyboard.press('Enter');
			const main = page.getByRole('main');
			await expect(
				main,
				`expected activating the skip link on ${path} at ${width}px to focus the page main. ` +
					'Builder: target the real <main> (tabindex="-1" plus fragment or focus call).'
			).toBeFocused();
			await expect(
				main.getByRole('heading', { level: 1 }),
				`expected the focused main on ${path} to be the actual page main with its h1. ` +
					'Builder: point the skip link at the page <main>, not a wrapper.'
			).toBeVisible();
			expect(page.url()).toContain(path);
			await expect(page.locator('main')).toHaveCount(1);
			await expect(
				page.locator('main main'),
				`expected no nested main landmarks on ${path}. Builder: keep a single <main>.`
			).toHaveCount(0);
		}
	});

	test(`[NAV-05 ${width}px] keyboard order is skip, nav links, Appearance, then content`, async ({
		page
	}) => {
		await page.setViewportSize({ width, height: 800 });
		await mockLibrary(page, 200, []);
		await page.goto('/library');
		await expectShellPresent(page, `/library at ${width}px`);

		await page.keyboard.press('Tab');
		await expect(skipLink(page)).toBeFocused();
		for (const [label, link] of [
			['Library', libraryLink(page)],
			['Select NZB locally', importLink(page)],
			['Account', accountLink(page)]
		] as const) {
			await page.keyboard.press('Tab');
			await expect(
				link,
				`expected Tab order skip, Library, Select NZB locally, Account at ${width}px; ` +
					`"${label}" was not next. Builder: keep nav links in DOM order after the skip link.`
			).toBeFocused();
		}
		// The existing Appearance control remains in the shell's native tab order.
		const appearance = page.getByRole('combobox', { name: 'Appearance', exact: true });
		await page.keyboard.press('Tab');
		await expect(
			appearance,
			`expected Appearance immediately after Account at ${width}px; ` +
				'Builder: preserve the existing selector in the shell keyboard order.'
		).toBeFocused();
		expect(await appearance.evaluate((element) => element.tagName)).toBe('SELECT');
		await page.keyboard.press('Home');
		await page.keyboard.press('ArrowDown');
		await page.keyboard.press('Enter');
		await expect(appearance).toHaveValue('light');
		// Dismiss any native option popup before testing forward traversal.
		await page.keyboard.press('Escape');
		await expect(appearance).toBeFocused();

		// One more Tab must enter the actual page main, not any arbitrary target.
		await page.keyboard.press('Tab');
		const inMain = await page.evaluate(() => {
			const main = document.querySelector('main');
			return !!main?.contains(document.activeElement);
		});
		expect(
			inMain,
			`expected keyboard focus to move into the page main immediately after Appearance at ${width}px. ` +
				'Builder: keep the shell before <main> in DOM order.'
		).toBe(true);
	});

	test(`[NAV-06 ${width}px] shell links show a visible focus ring`, async ({
		page
	}, testInfo) => {
		await page.setViewportSize({ width, height: 800 });
		await mockLibrary(page, 200, []);
		await page.goto('/library');
		await expectShellPresent(page, `/library at ${width}px`);
		// Baseline of the unfocused shell (also the RED-state evidence: no nav yet).
		await page.screenshot({ path: testInfo.outputPath(`shell-unfocused-${width}.png`) });

		await page.keyboard.press('Tab');
		await expect(skipLink(page)).toBeFocused();
		await expectSkipLinkRevealed(page, `/library at ${width}px`);
		for (const [label, link] of [
			['Library', libraryLink(page)],
			['Select NZB locally', importLink(page)],
			['Account', accountLink(page)]
		] as const) {
			await page.keyboard.press('Tab');
			await expect(link).toBeFocused();
			const ring = await link.evaluate((element) => {
				const style = getComputedStyle(element);
				return {
					focusVisible: element.matches(':focus-visible'),
					outlineStyle: style.outlineStyle,
					outlineWidth: style.outlineWidth
				};
			});
			expect(
				ring.focusVisible,
				`expected keyboard focus on "${label}" at ${width}px to match :focus-visible. ` +
					'Builder: keep the global focus-visible styling applying to shell links.'
			).toBe(true);
			expect(
				ring.outlineStyle,
				`expected a visible outline on focused "${label}" at ${width}px, got outline-style ` +
					`${ring.outlineStyle} ${ring.outlineWidth}. Builder: do not remove outlines.`
			).not.toBe('none');
			await page.screenshot({ path: testInfo.outputPath(`shell-focus-${label}-${width}.png`) });
		}
	});
}

for (const width of [390, 360]) {
	test(`[NAV-07 ${width}px] narrow width wraps with no overflow and no hamburger`, async ({
		page
	}, testInfo) => {
		await mockLibrary(page, 200, []);
		await mockAccountApis(page);
		await page.setViewportSize({ width, height: 800 });
		// Soft expects so one RED run reports every narrow destination; the
		// short timeout keeps the failure fast (passes resolve instantly).
		const SOFT_TIMEOUT = 1000;
		for (const path of ['/library', '/library/import', '/account/billing', '/']) {
			await page.goto(path);
			await page.screenshot({ path: testInfo.outputPath(`shell-${width}-${path.replaceAll('/', '_')}.png`) });
			const nav = shellNav(page);
			await expect.soft(
				nav,
				`expected the shell nav on ${path} at ${width}px. ${BUILDER}`
			).toHaveCount(1, { timeout: SOFT_TIMEOUT });
			for (const [label, link] of [
				['Library', libraryLink(page)],
				['Select NZB locally', importLink(page)],
				['Account', accountLink(page)]
			] as const) {
				// isVisible (no wait) keeps RED fast: boundingBox would retry
				// until the test timeout on a missing link.
				const visible = await link.isVisible();
				expect.soft(
					visible,
					`expected the "${label}" link visible without opening anything on ${path} at ${width}px ` +
						`(no hamburger). ${BUILDER}`
				).toBe(true);
				if (!visible) continue;
				const box = await link.boundingBox();
				expect.soft(
					box,
					`expected "${label}" to have a layout box on ${path} at ${width}px. ${BUILDER}`
				).not.toBeNull();
				if (box) {
					expect.soft(
						box.x >= -1 && box.x + box.width <= width + 1,
						`expected "${label}" (x=${box.x}, w=${box.width}) to fit inside ${width}px on ${path}. ` +
							'Builder: wrap the link row instead of overflowing.'
					).toBe(true);
				}
			}
			const overflow = await page.evaluate(() => ({
				scrollWidth: document.documentElement.scrollWidth,
				clientWidth: document.documentElement.clientWidth
			}));
			expect.soft(
				overflow.scrollWidth <= overflow.clientWidth + 1,
				`expected no horizontal overflow on ${path} at ${width}px ` +
					`(scrollWidth ${overflow.scrollWidth} vs clientWidth ${overflow.clientWidth}). ` +
					'Builder: use a wrapping layout.'
			).toBe(true);
			await expect.soft(nav.getByRole('button')).toHaveCount(0);
		}
	});
}

test('[NAV-08a] links stay discoverable with a populated library', async ({ page }) => {
	await mockLibrary(page, 200, LIBRARY_ITEMS);
	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toBeVisible();
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expectShellPresent(page, 'populated /library');
	for (const link of [libraryLink(page), importLink(page), accountLink(page)]) {
		await expect(link).toBeVisible();
	}
	await importLink(page).click();
	await expect(page).toHaveURL(/\/library\/import$/);
	await expect(page.getByTestId('import-file')).toBeVisible();
});

test('[NAV-08b] links stay discoverable with an empty library', async ({ page }) => {
	await mockLibrary(page, 200, []);
	await page.goto('/library');
	await expect(page.getByRole('main')).toContainText(/library is empty/i);
	await expectShellPresent(page, 'empty /library');
	for (const link of [libraryLink(page), importLink(page), accountLink(page)]) {
		await expect(link).toBeVisible();
	}
	await importLink(page).click();
	await expect(page).toHaveURL(/\/library\/import$/);
});

test('[NAV-08c] links stay discoverable with an unavailable library', async ({ page }) => {
	await mockLibrary(page, 404, { error: 'not found' });
	await page.goto('/library');
	await expect(page.getByRole('main')).toContainText(/not available yet/i);
	await expectShellPresent(page, 'unavailable /library');
	for (const link of [libraryLink(page), importLink(page), accountLink(page)]) {
		await expect(link).toBeVisible();
	}
	await importLink(page).click();
	await expect(page).toHaveURL(/\/library\/import$/);
});

test('[NAV-08d] links stay discoverable with a cached (stale) library', async ({ page }) => {
	let calls = 0;
	await page.route('**/api/library', (route) => {
		calls += 1;
		return calls === 1
			? route.fulfill({
					status: 200,
					contentType: 'application/json',
					body: JSON.stringify(LIBRARY_ITEMS)
				})
			: route.abort('failed');
	});
	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toBeVisible();
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-stale-badge')).toBeVisible();
	await expect(page.getByTestId('library-list')).toBeVisible();
	await expectShellPresent(page, 'cached /library');
	for (const link of [libraryLink(page), importLink(page), accountLink(page)]) {
		await expect(link).toBeVisible();
	}
	await importLink(page).click();
	await expect(page).toHaveURL(/\/library\/import$/);
});

test('[NAV-09] navigating emits no mutating or off-origin requests', async ({
	page,
	context
}, testInfo) => {
	await mockLibrary(page, 200, []);
	await mockAccountApis(page);
	const origin = new URL(testInfo.project.use?.baseURL ?? 'http://localhost:4173').origin;
	const seen: string[] = [];
	const mutations: string[] = [];
	const external: string[] = [];
	context.on('request', (request) => {
		const entry = `${request.method()} ${request.url()}`;
		seen.push(entry);
		if (!['GET', 'HEAD', 'OPTIONS'].includes(request.method())) mutations.push(entry);
		if (new URL(request.url()).origin !== origin) external.push(entry);
	});
	try {
		await page.goto('/');
		await expectShellPresent(page, '/');
		await libraryLink(page).click();
		await expect(page).toHaveURL(/\/library$/);
		await importLink(page).click();
		await expect(page).toHaveURL(/\/library\/import$/);
		await accountLink(page).click();
		await expect(page).toHaveURL(/\/account/);
		await libraryLink(page).click();
		await expect(page).toHaveURL(/\/library$/);
	} finally {
		await testInfo.attach('shell-navigation-network', {
			body: JSON.stringify({ mutations, external, seen }, null, 2),
			contentType: 'application/json'
		});
	}
	expect(
		mutations,
		`expected shell navigation to send zero mutating requests, saw: ${mutations.join('; ')}. ` +
			'Builder: navigation must never upload, process or post anything.'
	).toEqual([]);
	expect(
		external,
		`expected shell navigation to stay same-origin, saw: ${external.join('; ')}. ` +
			'Builder: the shell calls no backend or third party by itself.'
	).toEqual([]);
});

test('[NAV-10] demo player links carry explicit demo labeling, no playback claims', async ({
	page
}) => {
	await mockLibrary(page, 200, []);
	await page.goto('/');
	await expectShellPresent(page, '/');
	const links = await shellNav(page)
		.getByRole('link')
		.evaluateAll((elements) =>
			elements.map((element) => ({
				name: (element.textContent ?? '').replace(/\s+/g, ' ').trim(),
				href: element.getAttribute('href') ?? ''
			}))
		);
	for (const required of ['Library', 'Select NZB locally', 'Account']) {
		expect(
			links.some((link) => link.name === required),
			`expected a shell link named exactly "${required}", saw: ${JSON.stringify(links)}. ${BUILDER}`
		).toBe(true);
	}
	for (const link of links) {
		if (link.href.startsWith('/demo')) {
			expect(
				link.name,
				`expected the demo link (${link.href}) to say demo explicitly, got "${link.name}". ` +
					'Builder: label any synthetic-player link with the word demo.'
			).toMatch(/demo/i);
		}
		if (/play(ing|er)?|stream(ing)?/i.test(link.name)) {
			expect(
				link.name,
				`expected no working-playback promise in the shell nav, got "${link.name}". ` +
					'Builder: library playback is not wired; link the player only as an explicit demo.'
			).toMatch(/demo/i);
		}
	}
	const navText = await shellNav(page).innerText();
	expect(
		navText,
		'expected the shell nav to promise no upload or processing. ' +
			'Builder: selection stays local-only; keep those words out of the nav.'
	).not.toMatch(/upload|process/i);
});

test('[NAV-11] shell navigation preserves the local-only import disclosure', async ({
	page
}) => {
	await mockLibrary(page, 200, []);
	await page.goto('/library');
	await expectShellPresent(page, '/library');
	await importLink(page).click();
	await expect(page).toHaveURL(/\/library\/import$/);

	const input = page.getByTestId('import-file');
	await expect(input).toBeVisible();
	await expect(input).toHaveAttribute('type', 'file');
	await expect(input).toHaveAttribute('accept', '.nzb');
	await expect(page.getByTestId('import-status')).toHaveCount(0);
	const copy = await page.getByRole('main').innerText();
	expect(copy).toMatch(/\blocal(?:ly)?\b/i);
	expect(
		copy,
		'expected the import page reached via the shell to keep its no-upload disclosure. ' +
			'Builder: do not touch import copy or behavior.'
	).toMatch(/does not upload/i);

	await input.setInputFiles({
		name: 'shell-nav-local.nzb',
		mimeType: 'application/x-nzb',
		buffer: Buffer.from('<?xml version="1.0"?><nzb></nzb>')
	});
	const status = page.getByTestId('import-status');
	await expect(status).toBeVisible();
	await expect(status).toHaveAttribute('role', 'status');
	await expect(status).toContainText(/selected locally/i);
});

test('[NAV-12] account subnav keeps working alongside the shell nav', async ({ page }) => {
	await mockAccountApis(page);
	await page.goto('/account/billing');
	const subnav = page.getByRole('navigation', { name: 'Account', exact: true });
	await expect(
		subnav,
		'expected the existing account subnav on /account/billing. Builder: leave it untouched.'
	).toBeVisible();
	await expectShellPresent(page, '/account/billing');
	await expectCurrent(page, '/account/billing', 'Account');

	await subnav.getByRole('link', { name: 'Security' }).click();
	await expect(page).toHaveURL(/\/account\/security$/);
	await expect(subnav.getByRole('link', { name: 'Security' })).toHaveAttribute(
		'aria-current',
		'page'
	);
	await expect(subnav.locator('[aria-current="page"]')).toHaveCount(1);
	await expectCurrent(page, '/account/security (client nav)', 'Account');
});

test('[NAV-13] paywall close keeps focus restoration and modal isolation with nav', async ({
	page
}) => {
	let subscriptionCalls = 0;
	await page.route('**/v1/me/subscription', (route) => {
		subscriptionCalls += 1;
		return route.fulfill({
			status: 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '0' },
			body: JSON.stringify({ code: 'rate_limited' })
		});
	});
	await page.route('**/v1/me/usage', (route) =>
		route.fulfill({
			status: 500,
			contentType: 'application/json',
			body: JSON.stringify({ code: 'internal' })
		})
	);
	await mockLibrary(page, 200, []);
	await page.goto('/account/billing');
	const dialog = page.getByRole('dialog');
	await expect(
		dialog,
		'expected the 429 paywall dialog on /account/billing. Builder: leave paywall triggers alone.'
	).toBeVisible();
	await expectShellPresent(page, '/account/billing with paywall');

	// Modal keyboard isolation: Tab stays inside the native dialog.
	for (let tab = 0; tab < 6; tab += 1) {
		await page.keyboard.press('Tab');
		const inside = await page.evaluate(() => {
			const open = document.querySelector('[role="dialog"]');
			return !!open?.contains(document.activeElement);
		});
		expect(
			inside,
			'expected keyboard focus to stay inside the open paywall dialog (modal isolation). ' +
				'Builder: keep the native <dialog> trap; shell links stay inert while it is open.'
		).toBe(true);
	}

	// Reverse modal isolation: Shift+Tab wraps the first control to the last.
	const reverseCount = (await paywallFocusState(page)).count;
	expect(
		reverseCount,
		'expected at least two focusable controls in the paywall dialog for the Shift+Tab check. ' +
			'Builder: keep the upgrade link and dismiss actions rendered together.'
	).toBeGreaterThan(1);
	await page.evaluate(() => {
		const open = document.querySelector('[role="dialog"]');
		const controls = [
			...(open?.querySelectorAll<HTMLElement>('a[href], button:not(:disabled)') ?? [])
		].filter((control) => control.getClientRects().length > 0);
		controls[0]?.focus();
	});
	for (let tab = 0; tab < reverseCount; tab += 1) {
		await page.keyboard.press('Shift+Tab');
		const state = await paywallFocusState(page);
		expect(
			state.inside,
			'expected Shift+Tab to keep keyboard focus inside the open paywall dialog. ' +
				'Builder: keep the Shift+Tab wrap guard in the paywall focus trap.'
		).toBe(true);
		if (tab === 0) {
			expect(
				state.onLast,
				'expected Shift+Tab on the first paywall control to wrap focus to the last control. ' +
					'Builder: keep the Shift+Tab first-to-last wrap in the paywall focus trap.'
			).toBe(true);
		}
	}

	await dialog.getByRole('button', { name: 'Not now', exact: true }).click();
	await expect(dialog).toBeHidden();
	await expect(
		page.getByRole('main'),
		'expected paywall close to restore focus to the page main. ' +
			'Builder: preserve the root-layout focus restoration.'
	).toBeFocused();
	expect(subscriptionCalls).toBe(1);
	await expectCurrent(page, '/account/billing after paywall close', 'Account');

	await libraryLink(page).click();
	await expect(page).toHaveURL(/\/library$/);
	await expectCurrent(page, '/library after paywall close', 'Library');
});

test('[NAV-14] paywall upgrade navigation keeps the shell and a sane focus target', async ({
	page
}) => {
	await page.route('**/v1/me/subscription', (route) =>
		route.fulfill({
			status: 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '0' },
			body: JSON.stringify({ code: 'rate_limited' })
		})
	);
	await page.route('**/v1/me/usage', (route) =>
		route.fulfill({
			status: 500,
			contentType: 'application/json',
			body: JSON.stringify({ code: 'internal' })
		})
	);
	await page.goto('/account/billing');
	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await expectShellPresent(page, '/account/billing with paywall');

	await dialog.getByRole('link', { name: 'Upgrade your plan', exact: true }).click();
	await expect(page).toHaveURL(/\/pricing$/);
	await expect(
		page.getByRole('dialog'),
		'expected the paywall dialog gone after Upgrade navigation. Builder: keep upgrade behavior.'
	).toHaveCount(0);
	await expect(
		page.getByRole('main'),
		'expected focus on the destination main after Upgrade navigation. ' +
			'Builder: preserve the after-navigate focus restoration.'
	).toBeFocused();
	await expectShellPresent(page, '/pricing after upgrade');
	await expectCurrent(page, '/pricing after upgrade', null);
});

test('[NAV-15] existing player shortcuts keep working with the shell present', async ({
	page
}) => {
	await page.goto('/demo/player');
	await expect(
		page.getByTestId('queue-item'),
		'expected the 3-track demo queue. Builder: leave the demo player alone.'
	).toHaveCount(3);
	await expectShellPresent(page, '/demo/player');

	// A focused shell link is not a typing target: global transport keys stay live.
	await libraryLink(page).focus();
	await expect(libraryLink(page)).toBeFocused();
	await page.keyboard.press('n');
	await expect(
		page.getByTestId('now-playing'),
		'expected "n" to advance the demo track with a shell link focused. ' +
			'Builder: shell links must not capture or break global player keys.'
	).toContainText('Second Signal');

	await page.keyboard.press('?');
	const shortcuts = page.getByTestId('shortcuts-dialog');
	await expect(
		shortcuts,
		'expected "?" to open the shortcuts dialog with a shell link focused. ' +
			'Builder: keep the ? binding live outside inputs and modals.'
	).toBeVisible();
	await page.keyboard.press('Escape');
	await expect(shortcuts).toBeHidden();
});

test('[NAV-16] PWA head, landing honesty and login stay intact across navigation', async ({
	page
}) => {
	await mockLibrary(page, 200, []);
	await page.goto('/');
	await expectShellPresent(page, '/');
	await expect(
		page.locator('link[rel="manifest"]'),
		'expected the manifest link on /. Builder: keep the root-layout head entries.'
	).toHaveAttribute('href', '/manifest.webmanifest');
	await expect(page.locator('meta[name="theme-color"]')).toHaveAttribute('content', '#0b0d12');
	await expect(page.getByRole('main').locator('p').first()).toContainText(/not wired yet/i);
	await expectCurrent(page, '/', null);

	await libraryLink(page).click();
	await expect(page).toHaveURL(/\/library$/);
	await expect(
		page.locator('link[rel="manifest"]'),
		'expected the manifest link to survive client-side navigation. Builder: keep head in the layout.'
	).toHaveAttribute('href', '/manifest.webmanifest');
	await expect(page.locator('meta[name="theme-color"]')).toHaveAttribute('content', '#0b0d12');
	await expectCurrent(page, '/library (client nav)', 'Library');

	// Login redirects and form stay as they were; the shell only adds links.
	await page.goto('/login');
	await expect(page).toHaveURL(/\/login$/);
	await expect(page.getByLabel('Email', { exact: true })).toBeVisible();
	await expect(page.getByRole('button', { name: 'Sign in with passkey' })).toBeVisible();
	await expect(page.getByRole('button', { name: 'Email me a sign-in link' })).toBeVisible();
	await expectShellPresent(page, '/login');
	await expectCurrent(page, '/login', null);
});

test('[NAV-17] navigation adds no authenticated state and keeps sign-in recovery', async ({
	page
}) => {
	await mockLibrary(page, 200, []);
	await page.goto('/');
	await expectShellPresent(page, '/');
	const navText = await shellNav(page).innerText();
	expect(
		navText,
		'expected no sign-in/sign-out control inside the shell nav (no new auth state). ' +
			'Builder: link Library, Select NZB locally and Account only.'
	).not.toMatch(/sign[ -]?(in|out)/i);
	expect(
		navText,
		'expected no user identity inside the shell nav. Builder: add no auth-dependent content.'
	).not.toContain('@');

	await mockLibrary(page, 401, { error: 'unauthorized' });
	await page.goto('/library');
	const signIn = page.getByRole('link', { name: 'Sign in', exact: true });
	await expect(
		signIn,
		'expected the existing 401 sign-in recovery on /library. Builder: keep route auth behavior.'
	).toBeVisible();
	await expect(signIn).toHaveAttribute('href', '/login');
	await expectShellPresent(page, 'signed-out /library');
});

test('[NAV-18] native links plus design-token styling, no new route', async ({ page }) => {
	await mockLibrary(page, 200, []);
	await page.goto('/library');
	await expectShellPresent(page, '/library');

	const hrefs = await shellNav(page)
		.getByRole('link')
		.evaluateAll((elements) =>
			elements.map((element) => ({
				tag: element.tagName,
				href: element.getAttribute('href') ?? '',
				target: element.getAttribute('target')
			}))
		);
	expect(hrefs.length).toBeGreaterThanOrEqual(3);
	for (const link of hrefs) {
		expect(
			link.tag,
			`expected shell controls to be native links, got <${link.tag}>. Builder: use <a href>.`
		).toBe('A');
		expect(
			link.target,
			`expected shell link ${link.href} to navigate in place (no target). ` +
				'Builder: same-tab native navigation only.'
		).toBeNull();
		expect(
			link.href,
			`expected shell link hrefs to reuse existing routes only, got "${link.href}". ` +
				'Builder: no new route; link /library, /library/import, /account/* (and /demo/* labeled demo).'
		).toMatch(/^\/(library(\/import)?|account(\/.*)?|demo(\/.*)?)?$/);
	}

	const tokenHit = await page.evaluate(() => {
		const nav = document.querySelector('nav:has(a[href="/library/import"])');
		if (!nav) return false;
		for (const sheet of Array.from(document.styleSheets)) {
			let rules: CSSRuleList | null = null;
			try {
				rules = sheet.cssRules;
			} catch {
				continue;
			}
			if (!rules) continue;
			for (const rule of Array.from(rules)) {
				const styleRule = rule as CSSStyleRule;
				if (typeof styleRule.selectorText !== 'string') continue;
				if (!styleRule.cssText.includes('var(--nt-')) continue;
				try {
					if (nav.matches(styleRule.selectorText)) return true;
				} catch {
					// Selector valid in CSS but not for matches(); ignore.
				}
			}
		}
		return [nav, ...Array.from(nav.querySelectorAll('a'))].some((element) =>
			(element.getAttribute('style') ?? '').includes('var(--nt-')
		);
	});
	expect(
		tokenHit,
		'expected the shell nav styling to reuse design tokens (a var(--nt-*) rule matching the nav). ' +
			'Builder: style with the --nt-* tokens, not one-off values.'
	).toBe(true);
});
