import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * App-shell navigation acceptance (roadmap item NAV): reach the primary
 * destinations from a compact shell landmark without typing URLs.
 *
 * Protected Test Author handoff: the Builder never edits this file, with the
 * single pre-authorised mechanical exception under ACTIVATION below. Every
 * failing `expect` is the build contract; change production code (the app
 * shell), never these pins.
 *
 * ── Contract renegotiation for the app-shell redesign (design spec v1) ──
 *
 * The horizontal two-link shell (Library / Account)
 * is replaced by a three-pane app shell on desktop (top bar, left rail,
 * main view, optional right panel, player bar) and a mobile shell (compact
 * top bar, tab bar, mini-player). This file is the open renegotiation of
 * the protected NAV contract for that shell. It lands GREEN against the
 * current shell so the required web-E2E gate never goes red on main:
 *
 *   - Kept cases (NAV-03, 04, 06, 08a–d, 09, 11, 12, 13, 14, 15) keep their
 *     behavioural assertions and now locate shell furniture through
 *     generation-aware helpers, so they pass on the current shell and on
 *     the new shell with locator updates only — no Builder edit needed.
 *   - Changed cases (NAV-01, 02a–d, 05, 07, 10, 16, 17, 18) keep their
 *     current bodies, marked SUPERSEDED v1, and each gains a complete v2
 *     body below as `test.fixme`. The v2 bodies are the new contract.
 *   - NAV-03b (playback continuity), NAV-15 v2 (real-player shortcuts) and
 *     NAV-19 (player-bar hooks) activate later, with the player bar.
 *
 * ACTIVATION (the one pre-authorised Builder edit, app-shell build): in the
 * PR that ships the new shell, delete every test marked SUPERSEDED v1 and
 * remove `.fixme` from its v2 counterpart. Nothing else in this file may
 * change in that PR; any other change routes back to the test author. The
 * player-bar build does the same for NAV-03b, NAV-15 v1→v2 and NAV-19.
 * When the last v1 test is deleted, delete the legacy helpers and the v1
 * branch of the generation-aware helpers with them.
 *
 * Why each changed case changed (design spec v1 §14):
 *   NAV-01  the shell's job is now app navigation: the `Primary` landmark
 *           owns Home / Search / Your Library; Account moves to the account
 *           menu. User NZB import and its import-only Create menu retire.
 *           Buttons (back/forward/menus) legitimately exist, so
 *           "zero buttons in nav" becomes "every primary destination is a
 *           native link, visible without opening anything".
 *   NAV-02  marker rules generalise to the new route table; exact rules in
 *           the decision table below.
 *   NAV-05  tab order follows the new shell regions (design spec §11);
 *           Appearance keeps its control, values and keyboard behaviour —
 *           only its position pin moves (top bar, after the search field,
 *           before the account menu).
 *   NAV-07  principle kept (no hamburger, nothing gated, no overflow);
 *           shape becomes the mobile tab bar + mini-player.
 *   NAV-10  stronger: demo pages are unlinked from both shells, so the
 *           "a demo link may appear if labelled" tolerance is deleted.
 *   NAV-16  the landing page's "not wired yet" honesty copy retires when
 *           Home is real; manifest pin keeps, theme-color value is owned
 *           by the design tokens (format pinned here, value there).
 *   NAV-17  the app shell is the signed-in surface: an account menu (with
 *           sign-out inside it) is expected; no email or identity text in
 *           the always-visible chrome; the 401 recovery pin keeps.
 *   NAV-18  new routes are the point: the href allowlist becomes the
 *           design route table; native-link and design-token pins keep.
 *
 * Decisions recorded here (test author, within design spec v1):
 *   - Current-marker rules (longest pathname-boundary match wins):
 *       /home                → Home
 *       /search, /search/…   → Search
 *       /library, /library/… → Your Library (incl. liked, recent)
 *       /account/…           → no Primary marker; the "Account" entry of
 *                              the opened account menu carries
 *                              aria-current="page"; the account subnav
 *                              keeps its own marker (NAV-12)
 *       /settings, /settings/… → same shape, "Settings" menu entry
 *       /album/…, /artist/…, /playlist/…, /track/…, /genre/…,
 *       /section/…, /queue, /now-playing, everything else → no marker
 *     Public routes (/, /login, /pricing, /signup, /demo/…) render the
 *     public header, not the app shell: no `Primary` landmark there.
 *   - The account-menu button's accessible name is "Account menu"; the
 *     import-only Create control is retired. (Icon buttons are named by action.)
 *   - `data-testid="shell-topbar"` marks the top bar in BOTH shells (app
 *     and public header), so "some shell chrome is present" is testable
 *     on public routes too.
 *   - The app shell renders on app routes in these mocked, unauthenticated
 *     contexts exactly as the current shell does; shell rendering must not
 *     be gated on a session fetch this suite does not mock.
 *   - NAV-15 sequencing: the demo player carries the shortcut contract
 *     until the player bar graduates the demo logic to the real player;
 *     then NAV-15 v2 (real player) replaces v1. The full keyboard map is
 *     owned by the player suites; NAV-15 pins transport keys + guards
 *     with the shell present.
 *   - NAV-16's signed-in `/` → `/home` redirect is NOT pinned here: this
 *     suite has no authenticated fixture, and inventing a fake session to
 *     reach it would fake the auth boundary. It belongs to the Home
 *     build's own acceptance with a real signed-in fixture. The
 *     signed-out landing copy is a product decision; v2 pins head/PWA
 *     and login only.
 *
 * Test hooks (data-testid) — kept, mapped, new:
 *   kept:    library-item (library track rows), audio-player (the real
 *            <audio> element; loses native `controls` with the player
 *            bar), usage-meter (rail footer strip, or the library page
 *            panel when no rail — never two in the DOM)
 *   mapped:  now-playing — today the demo/queue readout; with the player
 *            bar it is the bar's now-playing identity cluster (the
 *            library page's library-now-playing retires into it)
 *            player-next — today the queue UI's next button; with the
 *            player bar it is the bar's Next transport button
 *   new:     shell-topbar, shell-rail, shell-tabbar, player-bar,
 *            player-play-pause, player-seek, player-volume, mini-player,
 *            right-panel, context-menu, toast (queue-panel already exists)
 *
 * Stable case IDs (v1 = superseded at activation, v2 = new contract):
 *   NAV-01  shell landmark + destination links            [v1 + v2]
 *   NAV-02a current marker: library family vs the rest    [v1 + v2]
 *   NAV-02b current marker: account                       [v1 + v2]
 *   NAV-02c current marker: nothing outside destinations  [v1 + v2]
 *   NAV-02d current marker: pathname-boundary matching    [v1 + v2]
 *   NAV-03  client-side navigation updates the marker without a reload
 *   NAV-03b playback survives client-side navigation      [v2, player bar]
 *   NAV-04  skip link reveals on focus and moves keyboard focus to main
 *   NAV-05  keyboard order through the shell              [v1 + v2]
 *   NAV-06  visible focus ring on shell controls (wide + narrow)
 *   NAV-07  narrow widths: no overflow, no hamburger      [v1 + v2]
 *   NAV-08a links stay discoverable with a populated library
 *   NAV-08b links stay discoverable with an empty library
 *   NAV-08c links stay discoverable with an unavailable library
 *   NAV-08d links stay discoverable with a cached (stale) library
 *   NAV-09  navigating emits no mutating or off-origin requests
 *   NAV-10  no demo/playback promises in shell chrome     [v1 + v2]
 *   NAV-11  retired local import: no shell entry, no route, no hooks
 *   NAV-12  account subnav keeps working alongside the shell nav
 *   NAV-13  paywall close keeps focus restoration and modal isolation (Tab + Shift+Tab) with nav
 *   NAV-14  paywall upgrade navigation keeps the shell and a sane focus target
 *   NAV-15  existing player shortcuts keep working with the shell present [v1 + v2]
 *   NAV-16  PWA head, landing and login stay intact       [v1 + v2]
 *   NAV-17  no identity in always-visible chrome; sign-in recovery keeps [v1 + v2]
 *   NAV-18  native links plus design-token styling, route allowlist [v1 + v2]
 *   NAV-19  player-bar hooks for the custom transport     [v2, player bar]
 *
 * No test imports project code, so a failure is never an import/compile
 * error. Each message states the expected UI and the Builder's next action.
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
const SHELL_BUILDER = 'Builder (app shell): see the v2 contract in this file header.';

/**
 * The shell nav, located by its landmark name. The old locator keyed on
 * owning the retired import link; the landmark is now
 * located the way assistive technology finds it. `getByRole` only matches
 * the accessibility tree, so a CSS-hidden per-breakpoint duplicate does
 * not double-count.
 */
function shellNav(page: Page): Locator {
	return page.getByRole('navigation', { name: 'Primary', exact: true });
}

// ── Legacy (v1) helpers: used ONLY by tests marked SUPERSEDED v1. They are
// deleted with those tests at activation. Kept tests use the
// generation-aware helpers further down.

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
	// EL TWO-5795 media-mock migration: PLAYABLE_ITEMS formerly fetched the
	// deleted /api/demo/audio; serve a local silent WAV so playback advances
	// without any synthetic-media server. Inert for non-playing tests.
	await page.route('**/shell-audio/**', (route) => {
		const wav = Buffer.alloc(44 + 16_000);
		wav.write('RIFF', 0);
		wav.writeUInt32LE(36 + 16_000, 4);
		wav.write('WAVEfmt ', 8);
		wav.writeUInt32LE(16, 16);
		wav.writeUInt16LE(1, 20);
		wav.writeUInt16LE(1, 22);
		wav.writeUInt32LE(8_000, 24);
		wav.writeUInt32LE(16_000, 28);
		wav.writeUInt16LE(2, 32);
		wav.writeUInt16LE(16, 34);
		wav.write('data', 36);
		wav.writeUInt32LE(16_000, 40);
		return route.fulfill({ status: 200, contentType: 'audio/wav', body: wav });
	});
}

/** Account pages without backend or paywall interference (no 402/429 here). */
async function mockAccountApis(page: Page): Promise<void> {
	await page.route('**/v1/me/*', (route) =>
		route.fulfill({ status: 503, contentType: 'application/json', body: '{}' })
	);
	await page.route('**/v1/me/subscription', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({
				state: 'no_subscription',
				planId: null,
				status: null,
				currentPeriodEnd: null,
				graceUntil: null,
				cancelAtPeriodEnd: false
			})
		})
	);
}

async function expectShellPresent(page: Page, where: string): Promise<void> {
	await expect(
		shellNav(page),
		`expected one app-shell nav landmark named "Primary" on ${where}; ` +
			`got none. ${BUILDER}`
	).toHaveCount(1, { timeout: PRESENCE_TIMEOUT });
}

// ── Generation-aware helpers (kept cases + all v2 cases) ──────────────
//
// "v1" is today's horizontal three-link shell; "v2" is the app-shell
// redesign. Kept cases assert behaviour that must hold on BOTH, so these
// helpers branch on the detected generation. The branch is transition
// scaffolding: when the last SUPERSEDED v1 test is deleted at activation,
// delete the v1 branches too. Headline shape (exact labels, markers, tab
// order, route allowlist) is pinned strictly by the v2 cases, never by
// these tolerant helpers.

/** Some shell chrome (app shell or public header) is present. */
async function expectShellChrome(page: Page, where: string): Promise<void> {
	await expect(
		page.getByTestId('shell-topbar'),
		`expected the shell top bar on ${where}. ${SHELL_BUILDER}`
	).toHaveCount(1, { timeout: PRESENCE_TIMEOUT });
}

/** The Library primary destination link ("Library" today, "Your Library" in v2). */
function libraryDestinationLink(page: Page): Locator {
	return shellNav(page).getByRole('link', { name: /^(?:Your )?Library$/ });
}

function homeDestinationLink(page: Page): Locator {
	return shellNav(page).getByRole('link', { name: 'Home', exact: true });
}

function searchDestinationLink(page: Page): Locator {
	return shellNav(page).getByRole('link', { name: 'Search', exact: true });
}

function accountMenuButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Account menu', exact: true });
}

/** Navigate to the account area through the shell. */
async function goToAccount(page: Page): Promise<void> {
	// Account lives in the top-bar account menu.
	await accountMenuButton(page).click();
	await page
		.getByRole('menuitem', { name: 'Account', exact: true })
		.or(page.getByRole('link', { name: 'Account', exact: true }))
		.first()
		.click();
}

type Destination = 'Home' | 'Search' | 'Library' | 'Account' | 'Settings';

/**
 * The current-marker contract, either generation. Exact v2 marker rules
 * are the decision table in the file header; Home/Search/Settings are
 * v2-only destinations (used by v2 cases).
 */
async function expectCurrentDestination(
	page: Page,
	path: string,
	expected: Destination | null
): Promise<void> {
	if (expected === null) {
		if ((await shellNav(page).count()) > 0) {
			await expect(
				currentMarkers(page),
				`expected no aria-current marker in the Primary nav on ${path}. ` +
					'Builder: mark only Home/Search/Your Library by pathname boundary.'
			).toHaveCount(0, { timeout: PRESENCE_TIMEOUT });
		}
		return;
	}
	if (expected === 'Account' || expected === 'Settings') {
		// Not a primary destination: the Primary nav stays unmarked and the
		// opened account menu's entry carries the marker instead.
		await expect(
			currentMarkers(page),
			`expected no Primary-nav marker on ${path} (${expected} is not a primary destination). ` +
				'Builder: carry the marker on the account-menu entry, not the rail/tab bar.'
		).toHaveCount(0, { timeout: PRESENCE_TIMEOUT });
		await accountMenuButton(page).click();
		const entry = page
			.getByRole('menuitem', { name: expected, exact: true })
			.or(page.getByRole('link', { name: expected, exact: true }))
			.first();
		await expect(
			entry,
			`expected the account menu's "${expected}" entry marked aria-current="page" on ${path}. ${SHELL_BUILDER}`
		).toHaveAttribute('aria-current', 'page', { timeout: PRESENCE_TIMEOUT });
		await page.keyboard.press('Escape');
		// The account subnav is a separate landmark: the shell must not steal its marker.
		if (path.startsWith('/account/')) {
			const subnav = page.getByRole('navigation', { name: 'Account', exact: true });
			await expect(
				subnav.locator('[aria-current="page"]'),
				`expected the account subnav to keep its own current marker on ${path}. ` +
					'Builder: leave the account subnav untouched.'
			).toHaveCount(1, { timeout: PRESENCE_TIMEOUT });
		}
		return;
	}
	const link =
		expected === 'Home'
			? homeDestinationLink(page)
			: expected === 'Search'
				? searchDestinationLink(page)
				: libraryDestinationLink(page);
	await expect(
		link,
		`expected the ${expected} destination (and only it) marked aria-current="page" on ${path}. ${SHELL_BUILDER}`
	).toHaveAttribute('aria-current', 'page', { timeout: PRESENCE_TIMEOUT });
	await expect(
		currentMarkers(page),
		`expected exactly one aria-current marker in the Primary nav on ${path}. ` +
			'Builder: mark only the longest pathname-boundary match.'
	).toHaveCount(1, { timeout: PRESENCE_TIMEOUT });
}

/** Shell destinations are reachable without typing a URL, either generation. */
async function expectShellDestinationsDiscoverable(page: Page, where: string): Promise<void> {
	await expectShellPresent(page, where);
	await expect(
		libraryDestinationLink(page),
		`expected the Library destination link visible on ${where}. ${BUILDER}`
	).toBeVisible({ timeout: PRESENCE_TIMEOUT });
	await expect(homeDestinationLink(page)).toBeVisible();
	await expect(searchDestinationLink(page)).toBeVisible();
	await expect(
		accountMenuButton(page),
		`expected the account-menu button visible on ${where}. ${SHELL_BUILDER}`
	).toBeVisible({ timeout: PRESENCE_TIMEOUT });
}

/** Tab forward until `target` holds focus (bounded); true when reached. */
async function tabUntilFocused(page: Page, target: Locator, maxTabs = 60): Promise<boolean> {
	for (let tab = 0; tab < maxTabs; tab += 1) {
		await page.keyboard.press('Tab');
		const focused = await target
			.evaluate((element) => element === document.activeElement)
			.catch(() => false);
		if (focused) return true;
	}
	return false;
}

/** Focus some shell link by keyboard traversal from the page start. */
async function focusShellLink(page: Page): Promise<void> {
	await page.keyboard.press('Tab'); // skip link
	// Probe presence first (the goToAccount pattern): evaluating
	// an absent locator inside tabUntilFocused waits out the timeout on
	// every tab, so on public-header routes (no Primary landmark, e.g.
	// public-header routes without it in v2) the fallback must be chosen up front, not after
	// 30 timed-out probes.
	let reached = false;
	if ((await libraryDestinationLink(page).count()) > 0) {
		reached = await tabUntilFocused(page, libraryDestinationLink(page), 30);
	}
	if (!reached) {
		// Public header (no Primary nav): its first link serves the same
		// "a focused shell link is not a typing target" purpose.
		reached = await tabUntilFocused(page, page.locator('header a[href]').first(), 30);
	}
	expect(reached, 'expected keyboard traversal to reach a shell link').toBe(true);
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
		const open = document.querySelector('dialog[open]');
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

test('[NAV-03] client-side navigation updates the marker without a reload', async ({ page }) => {
	await mockLibrary(page, 200, []);
	await mockAccountApis(page);
	// Starts on an app route (not `/`): in the new shell `/` is the public
	// landing without the Primary landmark. The journey — library → account, markers
	// following, no reload — preserves the surviving destinations.
	await page.goto('/library');
	await expectShellPresent(page, '/library');
	await expectCurrentDestination(page, '/library', 'Library');
	await page.evaluate(() => {
		(window as unknown as { __shellNavMarker: string }).__shellNavMarker = 'still-here';
	});

	await goToAccount(page);
	await expect(page).toHaveURL(/\/account/);
	await expect(
		page.getByRole('navigation', { name: 'Account', exact: true }),
		'expected the shell account path to land on a real account page showing the account subnav. ' +
			'Builder: point Account at an existing /account/* route (bare /account has no page).'
	).toBeVisible();
	await expectCurrentDestination(page, '/account (client nav)', 'Account');

	await libraryDestinationLink(page).click();
	await expect(page).toHaveURL(/\/library$/);
	await expectCurrentDestination(page, '/library (client nav, back)', 'Library');

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
		for (const path of ['/library', '/library/liked', '/account/billing']) {
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
		// Focus order itself is NAV-05's pin (it changes with the shell);
		// here each shell control is reached by tabbing forward, however
		// many stops its generation puts in between, and only the ring on
		// the focused control is asserted.
		const controls: ReadonlyArray<readonly [string, Locator]> = [
			['Home', homeDestinationLink(page)],
			['Search', searchDestinationLink(page)],
			['Your Library', libraryDestinationLink(page)]
		];
		for (const [label, link] of controls) {
			const reached = await tabUntilFocused(page, link, 40);
			expect(
				reached,
				`expected keyboard traversal to reach the "${label}" shell control at ${width}px. ${BUILDER}`
			).toBe(true);
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

test('[NAV-08a] links stay discoverable with a populated library', async ({ page }) => {
	await mockAccountApis(page);
	await mockLibrary(page, 200, LIBRARY_ITEMS);
	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toBeVisible();
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expectShellPresent(page, 'populated /library');
	await expectShellDestinationsDiscoverable(page, 'library state under test');
	await goToAccount(page);
	await expect(page).toHaveURL(/\/account\/billing$/);
});

test('[NAV-08b] links stay discoverable with an empty library', async ({ page }) => {
	await mockAccountApis(page);
	await mockLibrary(page, 200, []);
	await page.goto('/library');
	await expect(page.getByRole('main')).toContainText(/library is empty/i);
	await expectShellPresent(page, 'empty /library');
	await expectShellDestinationsDiscoverable(page, 'library state under test');
	await goToAccount(page);
	await expect(page).toHaveURL(/\/account\/billing$/);
});

test('[NAV-08c] links stay discoverable with an unavailable library', async ({ page }) => {
	await mockAccountApis(page);
	await mockLibrary(page, 404, { error: 'not found' });
	await page.goto('/library');
	await expect(page.getByRole('main')).toContainText(/not available yet/i);
	await expectShellPresent(page, 'unavailable /library');
	await expectShellDestinationsDiscoverable(page, 'library state under test');
	await goToAccount(page);
	await expect(page).toHaveURL(/\/account\/billing$/);
});

test('[NAV-08d] links stay discoverable with a cached (stale) library', async ({ page }) => {
	await mockAccountApis(page);
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
	await expectShellDestinationsDiscoverable(page, 'library state under test');
	await goToAccount(page);
	await expect(page).toHaveURL(/\/account\/billing$/);
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
		// App-route start (the new shell's `/` is the public landing).
		// Typing into the shell search field is deliberately NOT exercised
		// here: search-as-you-type fetches are search traffic, not
		// navigation; this case pins link/menu navigation only.
		await page.goto('/library');
		await expectShellPresent(page, '/library');
		await goToAccount(page);
		await expect(page).toHaveURL(/\/account/);
		await libraryDestinationLink(page).click();
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

for (const width of [1280, 390]) {
	test(`[NAV-11 ${width}px] retired local import: no shell entry, no route, no hooks`, async ({ page }) => {
		await page.setViewportSize({ width, height: 800 });
		await mockLibrary(page, 200, []);
		await page.goto('/library');
		await expectShellPresent(page, '/library');
		await expect(page.getByRole('main')).toContainText(/library is empty/i);

		const importName = /\bimport\b|select nzb/i;
		await expect(
			page.locator('a[href^="/library/import"]'),
			'expected no link to the retired import route anywhere on /library (shell or empty state). ' +
				'Builder: remove the import entry points.'
		).toHaveCount(0);
		await expect(shellNav(page).getByRole('link', { name: importName })).toHaveCount(0);
		await expect(page.getByRole('main').getByRole('link', { name: importName })).toHaveCount(0);
		await expect(page.getByRole('button', { name: 'Create', exact: true })).toHaveCount(0);

		for (const path of ['/library/import', '/library/import/extra']) {
			const response = await page.goto(path);
			expect(
				response?.status(),
				`expected ${path} to be gone (404). Builder: delete the import route.`
			).toBe(404);
			for (const hook of ['import-file', 'import-status']) {
				await expect(page.getByTestId(hook)).toHaveCount(0);
			}
		}
	});

}

test('[NAV-12] account subnav keeps working alongside the shell nav', async ({ page }) => {
	await mockAccountApis(page);
	await page.goto('/account/billing');
	const subnav = page.getByRole('navigation', { name: 'Account', exact: true });
	await expect(
		subnav,
		'expected the existing account subnav on /account/billing. Builder: leave it untouched.'
	).toBeVisible();
	await expectShellPresent(page, '/account/billing');
	await expectCurrentDestination(page, '/account/billing', 'Account');

	await subnav.getByRole('link', { name: 'Security' }).click();
	await expect(page).toHaveURL(/\/account\/security$/);
	await expect(subnav.getByRole('link', { name: 'Security' })).toHaveAttribute(
		'aria-current',
		'page'
	);
	await expect(subnav.locator('[aria-current="page"]')).toHaveCount(1);
	await expectCurrentDestination(page, '/account/security (client nav)', 'Account');
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
			const open = document.querySelector('dialog[open]');
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
		const open = document.querySelector('dialog[open]');
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
	await expectCurrentDestination(page, '/account/billing after paywall close', 'Account');

	await libraryDestinationLink(page).click();
	await expect(page).toHaveURL(/\/library$/);
	await expectCurrentDestination(page, '/library after paywall close', 'Library');
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
	await expectShellChrome(page, '/pricing after upgrade');
	await expectCurrentDestination(page, '/pricing after upgrade', null);
});

// ════════════════════════════════════════════════════════════════════════
// v2 contract — the app-shell redesign (design spec v1).
//
// Every test below is `test.fixme` on purpose: it pins UI that does not
// exist yet, and the file header's ACTIVATION step removes `.fixme` in
// the PR that builds that UI. Two gates:
//   [gate: app shell]  — activates with the three-pane/mobile shell build
//   [gate: player bar] — activates when the custom player bar replaces
//                        the native audio controls and graduates the
//                        demo player logic to the real player
// ════════════════════════════════════════════════════════════════════════

const PLAYABLE_ITEMS = [1, 2, 3].map((index) => ({
	id: `nav-play-${index}`,
	title: `Shell Song ${index}`,
	artist: 'Shell Band',
	// Both stream-url spellings: the fixture only needs the field the
	// graduated player reads; the other is ignored extra JSON.
	// EL TWO-5795: pointed at the route-mocked WAV in NAV-03b after the
	// synthetic-media endpoints were retired with the demo route.
	streamUrl: '/shell-audio/shell.wav',
	stream_url: '/shell-audio/shell.wav'
}));

/** Describe the currently focused element for tab-order recording. */
async function focusedDescriptor(page: Page): Promise<{
	tag: string;
	name: string;
	href: string;
	region: string;
}> {
	return page.evaluate(() => {
		const element = document.activeElement as HTMLElement | null;
		if (!element || element === document.body) {
			return { tag: 'BODY', name: '', href: '', region: 'body' };
		}
		const region = element.closest('[data-testid="player-bar"]')
			? 'player'
			: element.closest('[data-testid="mini-player"]')
				? 'mini-player'
				: element.closest('main')
					? 'main'
					: element.closest('[data-testid="shell-tabbar"]')
						? 'tabbar'
						: element.closest('nav[aria-label="Primary"]')
							? 'nav'
							: element.closest('[data-testid="shell-topbar"]')
								? 'topbar'
								: 'other';
		return {
			tag: element.tagName,
			name: (element.getAttribute('aria-label') ?? element.textContent ?? '')
				.replace(/\s+/g, ' ')
				.trim()
				.slice(0, 40),
			href: element.getAttribute('href') ?? '',
			region
		};
	});
}

/** Record up to `tabs` forward Tab stops from the current focus. */
async function recordTabSequence(
	page: Page,
	tabs: number
): Promise<Array<{ tag: string; name: string; href: string; region: string }>> {
	const sequence = [];
	for (let tab = 0; tab < tabs; tab += 1) {
		await page.keyboard.press('Tab');
		sequence.push(await focusedDescriptor(page));
	}
	return sequence;
}

// [gate: app shell]
test('[NAV-01 v2] Primary landmark owns Home, Search and Your Library as native links', async ({
	page
}) => {
	await page.setViewportSize({ width: 1280, height: 800 });
	await mockLibrary(page, 200, []);
	await page.goto('/library');
	await expectShellPresent(page, '/library');
	await expect(shellNav(page)).toHaveAccessibleName('Primary');
	await expect(
		page.getByTestId('shell-topbar'),
		`expected the app-shell top bar hook on /library. ${SHELL_BUILDER}`
	).toHaveCount(1, { timeout: PRESENCE_TIMEOUT });
	await expect(
		page.getByTestId('shell-rail'),
		`expected the left-rail hook at 1280px. ${SHELL_BUILDER}`
	).toHaveCount(1, { timeout: PRESENCE_TIMEOUT });

	for (const [label, link, href] of [
		['Home', homeDestinationLink(page), '/home'],
		['Search', searchDestinationLink(page), '/search'],
		['Your Library', libraryDestinationLink(page), '/library']
	] as const) {
		await expect(
			link,
			`expected the "${label}" primary destination visible at 1280px without opening anything. ${SHELL_BUILDER}`
		).toBeVisible({ timeout: PRESENCE_TIMEOUT });
		await expect(link).toHaveAttribute('href', href);
		const tag = await link.evaluate((element) => element.tagName);
		expect(
			tag,
			`expected the "${label}" destination to be a native link, got <${tag}>. ` +
				'Builder: primary destinations are plain <a href>, never buttons.'
		).toBe('A');
	}
	await expect(
		accountMenuButton(page),
		`expected the account-menu button in the top bar. ${SHELL_BUILDER}`
	).toBeVisible({ timeout: PRESENCE_TIMEOUT });

	// Icon-rail widths: labels may collapse to icons, but every destination
	// stays a visible link with its accessible name.
	await page.setViewportSize({ width: 800, height: 800 });
	for (const link of [
		homeDestinationLink(page),
		searchDestinationLink(page),
		libraryDestinationLink(page)
	]) {
		await expect(link).toBeVisible();
	}
});

// [gate: app shell]
test('[NAV-02a v2] direct load marks Home, Search and Your Library by route segment', async ({
	page
}) => {
	await mockLibrary(page, 200, []);
	await mockAccountApis(page);
	const cases: Array<readonly [string, Destination]> = [
		['/home', 'Home'],
		['/search', 'Search'],
		['/search/shell%20band', 'Search'],
		['/library', 'Library'],
		['/library/liked', 'Library'],
		['/library/recent', 'Library']
	];
	for (const [path, destination] of cases) {
		await page.goto(path);
		await expectShellPresent(page, path);
		await expectCurrentDestination(page, path, destination);
	}
});

// [gate: app shell]
test('[NAV-02b v2] account and settings pages mark the account-menu entry, not the rail', async ({
	page
}) => {
	await mockLibrary(page, 200, []);
	await mockAccountApis(page);
	for (const path of ['/account/billing', '/account/security']) {
		await page.goto(path);
		await expectShellPresent(page, path);
		await expectCurrentDestination(page, path, 'Account');
	}
	await page.goto('/settings');
	await expectShellPresent(page, '/settings');
	await expectCurrentDestination(page, '/settings', 'Settings');
});

// [gate: app shell]
test('[NAV-02c v2] entity and look-alike routes mark no primary destination', async ({
	page
}) => {
	await mockLibrary(page, 200, []);
	await mockAccountApis(page);
	// Entity routes: shell present, no primary marker (context is shown by
	// the page itself, not by lighting a destination).
	for (const path of ['/album/abc', '/artist/abc', '/playlist/abc', '/track/abc']) {
		await page.goto(path);
		await expectShellPresent(page, path);
		await expectCurrentDestination(page, path, null);
	}
	// Boundary look-alikes: never a marker, whatever shell the error page uses.
	for (const path of ['/library2', '/searchfoo', '/home2', '/accountant']) {
		await page.goto(path);
		await expectCurrentDestination(page, path, null);
	}
});

// [gate: app shell]
test('[NAV-02d v2] nested paths match by pathname boundary', async ({ page }) => {
	await mockLibrary(page, 200, []);
	await page.goto('/library/liked/extra');
	await expectShellPresent(page, '/library/liked/extra');
	await expectCurrentDestination(page, '/library/liked/extra', 'Library');

	await page.goto('/search/shell%20band/tracks');
	await expectCurrentDestination(page, '/search/shell band/tracks', 'Search');
});

// [gate: player bar]
test('[NAV-03b v2] playback state survives client-side navigation', async ({ page }) => {
	await mockLibrary(page, 200, PLAYABLE_ITEMS);
	await page.goto('/library');
	await page.getByTestId('library-item').first().click();
	const player = page.getByTestId('audio-player');
	await expect(
		page.getByTestId('now-playing'),
		'expected the player bar to show the selected track'
	).toContainText('Shell Song 1', { timeout: PRESENCE_TIMEOUT });

	const state = () =>
		player.evaluate((element) => {
			const audio = element as HTMLAudioElement & { __navMark?: string };
			return { time: audio.currentTime, paused: audio.paused, mark: audio.__navMark ?? null };
		});
	if ((await state()).paused) {
		await page.getByTestId('player-play-pause').click();
	}
	await expect
		.poll(async () => (await state()).time, {
			timeout: 10_000,
			message: 'expected playback to advance before navigating away'
		})
		.toBeGreaterThan(0);
	await page.getByTestId('player-play-pause').click();
	await expect.poll(async () => (await state()).paused).toBe(true);
	await player.evaluate((element) => {
		(element as HTMLAudioElement & { __navMark?: string }).__navMark = 'same-element';
	});
	const pausedAt = (await state()).time;

	await homeDestinationLink(page).click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(
		page.getByTestId('player-bar'),
		'expected the player bar to persist across client navigation. ' +
			'Builder: the player lives in the shell, not in the routed page.'
	).toBeVisible();
	await libraryDestinationLink(page).click();
	await expect(page).toHaveURL(/\/library$/);

	const after = await state();
	expect(
		after.mark,
		'expected the SAME <audio> element after navigating away and back (no remount). ' +
			'Builder: keep one audio element in the shell for the whole session.'
	).toBe('same-element');
	expect(
		Math.abs(after.time - pausedAt),
		`expected the paused position to survive navigation (was ${pausedAt}, now ${after.time})`
	).toBeLessThanOrEqual(0.25);
	await expect(page.getByTestId('now-playing')).toContainText('Shell Song 1');
});

// [gate: app shell]
for (const width of [1280, 390]) {
	test(`[NAV-04 v2 ${width}px] skip link moves keyboard focus to the page main`, async ({
		page
	}) => {
		await page.setViewportSize({ width, height: 800 });
		await mockLibrary(page, 200, []);
		await mockAccountApis(page);
		for (const path of ['/home', '/library', '/account/billing']) {
			await page.goto(path);
			await expect(
				skipLink(page),
				`expected a "Skip to content" link on ${path} at ${width}px. ${SHELL_BUILDER}`
			).toHaveCount(1, { timeout: PRESENCE_TIMEOUT });
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
				`expected activating the skip link on ${path} at ${width}px to focus the page main.`
			).toBeFocused();
			await expect(page.locator('main')).toHaveCount(1);
			await expect(page.locator('main main')).toHaveCount(0);
		}
	});
}

// [gate: app shell]
test('[NAV-05 v2 1280px] keyboard order is skip, top bar, rail, main, then player', async ({
	page
}) => {
	await page.setViewportSize({ width: 1280, height: 800 });
	await mockLibrary(page, 200, []);
	await page.goto('/library');
	await expectShellPresent(page, '/library at 1280px');

	const sequence = await recordTabSequence(page, 45);
	const indexOf = (predicate: (stop: (typeof sequence)[number]) => boolean) =>
		sequence.findIndex(predicate);
	const skip = indexOf((stop) => stop.href.startsWith('#'));
	const topbarHome = indexOf((stop) => stop.region === 'topbar' && stop.href === '/home');
	const searchField = indexOf((stop) => stop.region === 'topbar' && stop.tag === 'INPUT');
	const appearance = indexOf((stop) => stop.tag === 'SELECT');
	const account = indexOf((stop) => stop.name === 'Account menu');
	const railHome = indexOf((stop) => stop.region === 'nav' && stop.href === '/home');
	const railSearch = indexOf((stop) => stop.region === 'nav' && stop.href === '/search');
	const railLibrary = indexOf((stop) => stop.region === 'nav' && stop.href === '/library');
	const firstMain = indexOf((stop) => stop.region === 'main');
	const firstPlayer = indexOf((stop) => stop.region === 'player');

	expect(skip, 'expected the skip link to be the first Tab stop').toBe(0);
	for (const [label, index] of [
		['top-bar Home', topbarHome],
		['search field', searchField],
		['Appearance select', appearance],
		['Account menu button', account],
		['rail Home', railHome],
		['rail Search', railSearch],
		['rail Your Library', railLibrary],
		['first main stop', firstMain]
	] as const) {
		expect(index, `expected Tab order to reach ${label}; sequence: ${JSON.stringify(sequence)}`).toBeGreaterThan(-1);
	}
	expect(topbarHome).toBeLessThan(searchField);
	expect(searchField, 'Appearance sits after the search field (its only moved pin)').toBeLessThan(appearance);
	expect(appearance, 'Appearance sits before the account menu').toBeLessThan(account);
	expect(account, 'rail destinations follow the top bar').toBeLessThan(railHome);
	expect(railHome).toBeLessThan(railSearch);
	expect(railSearch).toBeLessThan(railLibrary);
	expect(railLibrary, 'main content follows the rail').toBeLessThan(firstMain);
	if (firstPlayer > -1) {
		expect(firstMain, 'the player bar is reached only after main content').toBeLessThan(firstPlayer);
	}
	// Back/Forward, when enabled, precede the top-bar Home link.
	const back = indexOf((stop) => stop.name === 'Back');
	if (back > -1) expect(back).toBeLessThan(topbarHome);

	// The Appearance control keeps its exact keyboard behaviour (v1 pin).
	await page.goto('/library');
	const appearanceSelect = page.getByRole('combobox', { name: 'Appearance', exact: true });
	expect(await tabUntilFocused(page, appearanceSelect, 20)).toBe(true);
	expect(await appearanceSelect.evaluate((element) => element.tagName)).toBe('SELECT');
	await page.keyboard.press('Home');
	await page.keyboard.press('ArrowDown');
	await page.keyboard.press('Enter');
	await expect(appearanceSelect).toHaveValue('light');
});

// [gate: app shell]
test('[NAV-05 v2 390px] keyboard order is skip, top bar, main, mini-player, tab bar', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 800 });
	await mockLibrary(page, 200, []);
	await page.goto('/library');
	await expectShellPresent(page, '/library at 390px');

	const sequence = await recordTabSequence(page, 80);
	const indexOf = (predicate: (stop: (typeof sequence)[number]) => boolean) =>
		sequence.findIndex(predicate);
	expect(indexOf((stop) => stop.href.startsWith('#')), 'skip link first').toBe(0);
	const firstMain = indexOf((stop) => stop.region === 'main');
	const firstMini = indexOf((stop) => stop.region === 'mini-player');
	const tabHome = indexOf((stop) => stop.region === 'tabbar' && stop.href === '/home');
	const tabSearch = indexOf((stop) => stop.region === 'tabbar' && stop.href === '/search');
	const tabLibrary = indexOf((stop) => stop.region === 'tabbar' && stop.href === '/library');
	for (const [label, index] of [
		['first main stop', firstMain],
		['tab-bar Home', tabHome],
		['tab-bar Search', tabSearch],
		['tab-bar Your Library', tabLibrary]
	] as const) {
		expect(index, `expected Tab order to reach ${label}; sequence: ${JSON.stringify(sequence)}`).toBeGreaterThan(-1);
	}
	expect(firstMain, 'main is reached before the tab bar').toBeLessThan(tabHome);
	if (firstMini > -1) {
		expect(firstMain, 'mini-player follows main').toBeLessThan(firstMini);
		expect(firstMini, 'tab bar is the last region').toBeLessThan(tabHome);
	}
	expect(tabHome).toBeLessThan(tabSearch);
	expect(tabSearch).toBeLessThan(tabLibrary);
	expect(
		indexOf((stop) => stop.region === 'player'),
		'the desktop player bar must not be a tab stop region on mobile'
	).toBe(-1);
});

// [gate: app shell]
test('[NAV-06 v2 390px] tab-bar links show a visible focus ring', async ({ page }, testInfo) => {
	await page.setViewportSize({ width: 390, height: 800 });
	await mockLibrary(page, 200, []);
	await page.goto('/library');
	await expectShellPresent(page, '/library at 390px');
	const link = searchDestinationLink(page);
	expect(await tabUntilFocused(page, link, 80)).toBe(true);
	const ring = await link.evaluate((element) => {
		const style = getComputedStyle(element);
		return { focusVisible: element.matches(':focus-visible'), outlineStyle: style.outlineStyle };
	});
	expect(ring.focusVisible, 'expected :focus-visible on the focused tab-bar link').toBe(true);
	expect(ring.outlineStyle, 'expected a visible outline on the focused tab-bar link').not.toBe('none');
	await page.screenshot({ path: testInfo.outputPath('shell-focus-tabbar-search-390.png') });
});

// [gate: player bar]
test('[NAV-06 v2 1280px] transport buttons show a visible focus ring', async ({ page }, testInfo) => {
	await page.setViewportSize({ width: 1280, height: 800 });
	await mockLibrary(page, 200, PLAYABLE_ITEMS);
	await page.goto('/library');
	await page.getByTestId('library-item').first().click();
	const playPause = page.getByTestId('player-play-pause');
	await expect(playPause).toBeVisible({ timeout: PRESENCE_TIMEOUT });
	expect(await tabUntilFocused(page, playPause, 80)).toBe(true);
	const ring = await playPause.evaluate((element) => {
		const style = getComputedStyle(element);
		return { focusVisible: element.matches(':focus-visible'), outlineStyle: style.outlineStyle };
	});
	expect(ring.focusVisible, 'expected :focus-visible on the focused transport button').toBe(true);
	expect(ring.outlineStyle, 'expected a visible outline on the focused transport button').not.toBe('none');
	await page.screenshot({ path: testInfo.outputPath('shell-focus-play-pause-1280.png') });
});

// [gate: app shell]
for (const width of [390, 360]) {
	test(`[NAV-07 v2 ${width}px] tab bar fits the viewport with no hamburger and no overflow`, async ({
		page
	}, testInfo) => {
		await mockLibrary(page, 200, []);
		await mockAccountApis(page);
		await page.setViewportSize({ width, height: 800 });
		const SOFT_TIMEOUT = 1000;
		for (const path of ['/library', '/home', '/account/billing']) {
			await page.goto(path);
			await page.screenshot({ path: testInfo.outputPath(`shell-v2-${width}-${path.replaceAll('/', '_')}.png`) });
			const tabbar = page.getByTestId('shell-tabbar');
			await expect.soft(
				tabbar,
				`expected the mobile tab bar on ${path} at ${width}px. ${SHELL_BUILDER}`
			).toHaveCount(1, { timeout: SOFT_TIMEOUT });
			for (const [label, link] of [
				['Home', homeDestinationLink(page)],
				['Search', searchDestinationLink(page)],
				['Your Library', libraryDestinationLink(page)]
			] as const) {
				const visible = await link.isVisible();
				expect.soft(
					visible,
					`expected the "${label}" tab visible without opening anything on ${path} at ${width}px (no hamburger).`
				).toBe(true);
				if (!visible) continue;
				const box = await link.boundingBox();
				expect.soft(box).not.toBeNull();
				if (box) {
					expect.soft(
						box.x >= -1 && box.x + box.width <= width + 1,
						`expected the "${label}" tab (x=${box.x}, w=${box.width}) inside ${width}px`
					).toBe(true);
					expect.soft(
						box.height >= 47,
						`expected the "${label}" tab target at least 48px tall, got ${box.height}`
					).toBe(true);
				}
			}
			await expect.soft(
				tabbar.getByRole('button'),
				'expected no menu/hamburger button in the tab bar gating a destination'
			).toHaveCount(0);
			const overflow = await page.evaluate(() => ({
				scrollWidth: document.documentElement.scrollWidth,
				clientWidth: document.documentElement.clientWidth
			}));
			expect.soft(
				overflow.scrollWidth <= overflow.clientWidth + 1,
				`expected no horizontal overflow on ${path} at ${width}px`
			).toBe(true);
			// Mini-player (only when a track is loaded) sits above the tab
			// bar; it must never cover it.
			const mini = page.getByTestId('mini-player');
			if ((await mini.count()) > 0 && (await mini.isVisible())) {
				const miniBox = await mini.boundingBox();
				const tabBox = await tabbar.boundingBox();
				if (miniBox && tabBox) {
					expect.soft(
						miniBox.y + miniBox.height <= tabBox.y + 1,
						'expected the mini-player to end where the tab bar starts (no overlap)'
					).toBe(true);
				}
			}
		}
	});
}

// [gate: app shell]
test('[NAV-10 v2] no demo links and no playback promises in either shell', async ({ page }) => {
	await mockLibrary(page, 200, []);
	await page.goto('/library');
	await expectShellPresent(page, '/library');
	const links = await page
		.locator('[data-testid="shell-topbar"] a[href], nav[aria-label="Primary"] a[href]')
		.evaluateAll((elements) =>
			elements.map((element) => ({
				name: (element.textContent ?? '').replace(/\s+/g, ' ').trim(),
				href: element.getAttribute('href') ?? ''
			}))
		);
	for (const required of ['Home', 'Search', 'Your Library']) {
		expect(
			links.some((link) => link.name.includes(required)),
			`expected a shell link for "${required}", saw: ${JSON.stringify(links)}`
		).toBe(true);
	}
	for (const link of links) {
		expect(
			link.href.startsWith('/demo'),
			`expected NO demo link anywhere in the shell chrome, found ${link.href} ("${link.name}"). ` +
				'Builder: demo pages stay unlinked; the labelled-demo tolerance is retired.'
		).toBe(false);
	}
	const chromeText = await page
		.locator('[data-testid="shell-topbar"], nav[aria-label="Primary"]')
		.evaluateAll((elements) => elements.map((element) => (element as HTMLElement).innerText).join('\n'));
	expect(
		chromeText,
		'expected the shell chrome to promise no upload or processing (selection stays local-only)'
	).not.toMatch(/upload|process/i);
});

// [gate: player bar]
test('[NAV-15 v2] player shortcuts work on the real player with the shell present', async ({
	page
}) => {
	await mockLibrary(page, 200, PLAYABLE_ITEMS);
	await page.goto('/library');
	await page.getByTestId('library-item').first().click();
	await expect(page.getByTestId('now-playing')).toContainText('Shell Song 1', {
		timeout: PRESENCE_TIMEOUT
	});
	// One row click queues ONE track (append-only selection: selectTrack →
	// addTrack keeps the current index), so queue the second track
	// explicitly — "n" must have a next track to advance to. Queueing it
	// must not steal current: now-playing stays on Shell Song 1 until "n".
	await page.getByTestId('library-item').nth(1).click();
	await expect(page.getByTestId('library-queued-badge')).toHaveCount(2, {
		timeout: PRESENCE_TIMEOUT
	});
	await expect(page.getByTestId('now-playing')).toContainText('Shell Song 1');

	// A focused shell link is not a typing target: global transport keys stay live.
	await libraryDestinationLink(page).focus();
	await page.keyboard.press('n');
	await expect(
		page.getByTestId('now-playing'),
		'expected "n" to advance the real player with a shell link focused. ' +
			'Builder: shell links must not capture or break global player keys.'
	).toContainText('Shell Song 2', { timeout: PRESENCE_TIMEOUT });

	await page.keyboard.press('?');
	const shortcuts = page.getByTestId('shortcuts-dialog');
	await expect(shortcuts, 'expected "?" to open the shortcuts dialog').toBeVisible();
	await page.keyboard.press('Escape');
	await expect(shortcuts).toBeHidden();

	// Typing guard: keys typed into the top-bar search field are text,
	// never transport shortcuts.
	const searchField = page.locator('[data-testid="shell-topbar"] input').first();
	await searchField.click();
	await page.keyboard.press('n');
	await expect(searchField).toHaveValue(/n/);
	await expect(
		page.getByTestId('now-playing'),
		'expected typing "n" in the search field NOT to advance the track. ' +
			'Builder: keep the shortcuts typing guard (inputs are never transport targets).'
	).toContainText('Shell Song 2');
});

// [gate: app shell]
test('[NAV-16 v2] PWA head and login stay intact; public routes use the public header', async ({
	page
}) => {
	await mockLibrary(page, 200, []);
	await page.goto('/');
	await expect(
		page.locator('link[rel="manifest"]'),
		'expected the manifest link on /. Builder: keep the root-layout head entries.'
	).toHaveAttribute('href', '/manifest.webmanifest');
	// The theme-color VALUE is owned by the design tokens; this pin keeps
	// the meta present and well-formed whatever value the tokens choose.
	await expect(page.locator('meta[name="theme-color"]')).toHaveAttribute('content', /^#[0-9a-f]{6}$/i);
	await expect(
		shellNav(page),
		'expected no Primary (app-shell) landmark on the public landing. ' +
			'Builder: public routes render the public header, not the app shell.'
	).toHaveCount(0);
	await expectShellChrome(page, '/');

	await page.goto('/library');
	await expectShellPresent(page, '/library');
	await expect(page.locator('link[rel="manifest"]')).toHaveAttribute('href', '/manifest.webmanifest');
	await expectCurrentDestination(page, '/library', 'Library');

	await page.goto('/login');
	await expect(page).toHaveURL(/\/login$/);
	await expect(page.getByLabel('Email', { exact: true })).toBeVisible();
	await expect(page.getByRole('button', { name: 'Sign in with passkey' })).toBeVisible();
	await expect(page.getByRole('button', { name: 'Email me a sign-in link' })).toBeVisible();
	await expect(shellNav(page)).toHaveCount(0);
});

// [gate: app shell]
test('[NAV-17 v2] no identity in always-visible chrome and sign-in recovery keeps', async ({
	page
}) => {
	await mockLibrary(page, 200, []);
	await page.goto('/library');
	await expectShellPresent(page, '/library');
	// The app shell is the signed-in surface and an account menu is
	// expected — but no email/identity text, and no sign-out control,
	// in the always-visible chrome (menu closed).
	const topbarText = await page.getByTestId('shell-topbar').innerText();
	expect(
		topbarText,
		'expected no email address in the always-visible top bar. ' +
			'Builder: the account affordance is an initial/avatar + "Account menu" name only.'
	).not.toContain('@');
	await expect(
		page.getByRole('button', { name: /sign[ -]?out/i }),
		'expected no visible sign-out control while the account menu is closed'
	).toHaveCount(0);
	await expect(accountMenuButton(page)).toBeVisible();

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

// [gate: app shell]
test('[NAV-18 v2] native links plus design-token styling, design route-table allowlist', async ({
	page
}) => {
	await mockLibrary(page, 200, []);
	await page.goto('/library');
	await expectShellPresent(page, '/library');

	const hrefs = await page
		.locator('[data-testid="shell-topbar"] a[href], nav[aria-label="Primary"] a[href]')
		.evaluateAll((elements) =>
			elements.map((element) => ({
				tag: element.tagName,
				href: element.getAttribute('href') ?? '',
				target: element.getAttribute('target')
			}))
		);
	expect(hrefs.length).toBeGreaterThanOrEqual(3);
	// The design route table (design spec v1 §2). Demo routes are absent
	// on purpose: they are unlinked (NAV-10 v2).
	const ROUTE_TABLE =
		/^\/(home|search(\/.*)?|library(\/.*)?|artist\/[^/]+(\/.*)?|album\/[^/]+|playlist\/[^/]+|track\/[^/]+|genre\/[^/]+|section\/[^/]+|queue|now-playing|settings(\/.*)?|account(\/.*)?|pricing|login|signup|auth\/magic)?$/;
	for (const link of hrefs) {
		expect(
			link.tag,
			`expected shell controls to be native links, got <${link.tag}>. Builder: use <a href>.`
		).toBe('A');
		expect(
			link.target,
			`expected shell link ${link.href} to navigate in place (no target)`
		).toBeNull();
		expect(
			link.href,
			`expected shell link hrefs to stay within the design route table, got "${link.href}". ` +
				'Builder: link only routes from the route table; a new route is a design change first.'
		).toMatch(ROUTE_TABLE);
	}

	const tokenHit = await page.evaluate(() => {
		const navs = Array.from(document.querySelectorAll('nav[aria-label="Primary"]'));
		if (navs.length === 0) return false;
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
					if (navs.some((nav) => nav.matches(styleRule.selectorText))) return true;
				} catch {
					// Selector valid in CSS but not for matches(); ignore.
				}
			}
		}
		return navs.some((nav) =>
			[nav, ...Array.from(nav.querySelectorAll('a'))].some((element) =>
				(element.getAttribute('style') ?? '').includes('var(--nt-')
			)
		);
	});
	expect(
		tokenHit,
		'expected the shell nav styling to reuse design tokens (a var(--nt-*) rule matching the nav). ' +
			'Builder: style with the --nt-* tokens, not one-off values.'
	).toBe(true);
});

// [gate: player bar]
test('[NAV-19 v2] player bar exposes the custom-transport hooks', async ({ page }) => {
	await mockLibrary(page, 200, PLAYABLE_ITEMS);
	await page.goto('/library');
	await page.getByTestId('library-item').first().click();

	await expect(
		page.getByTestId('player-bar'),
		`expected the persistent player bar after selecting a track. ${SHELL_BUILDER}`
	).toBeVisible({ timeout: PRESENCE_TIMEOUT });
	const audio = page.getByTestId('audio-player');
	await expect(audio, 'expected the real <audio> element to keep the audio-player hook').toHaveCount(1);
	const hasControls = await audio.evaluate((element) => (element as HTMLAudioElement).controls);
	expect(
		hasControls,
		'expected NO native controls on the audio element (the custom bar replaces them). ' +
			'Builder: render <audio> without `controls`; transport lives in the player bar.'
	).toBe(false);

	const seek = page.getByTestId('player-seek');
	await expect(seek, 'expected the custom seek slider hook').toBeVisible();
	const seekTag = await seek.evaluate((element) => ({
		tag: element.tagName,
		type: element.getAttribute('type'),
		role: element.getAttribute('role')
	}));
	expect(
		seekTag.tag === 'INPUT' && seekTag.type === 'range',
		`expected player-seek to be a native range input (slider semantics), got ${JSON.stringify(seekTag)}`
	).toBe(true);

	await expect(page.getByTestId('player-play-pause')).toBeVisible();
	await expect(
		page.getByTestId('player-play-pause'),
		'expected the play/pause button to be named by its action'
	).toHaveAccessibleName(/play|pause/i);
	await expect(page.getByTestId('player-next'), 'expected the Next transport hook').toBeVisible();
	await expect(page.getByTestId('now-playing')).toContainText('Shell Song 1');
	// Never two usage meters (rail strip XOR page panel).
	await expect(page.getByTestId('usage-meter')).toHaveCount(1);
});
