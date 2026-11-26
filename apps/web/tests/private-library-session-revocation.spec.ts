import { expect, type Page } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('a late successful Library refresh cannot publish tracks after session revocation', async ({ page, browser, origin }) => {
	const { library } = PRIVATE_MUSIC;
	const libraryUrl = origin + library.path;
	const listingUrl = origin + library.api;
	const verifier = '/v1/me/likes/session';
	const invitee = { cookie: 'synthetic-invitee', binding: 'A'.repeat(43) };
	const active = { cookie: 'synthetic-active-invitee', binding: 'E'.repeat(43) };
	const sessions = new Set([invitee.cookie, active.cookie]);
	const track = (id: number, title: string) => ({ id: `00000000-0000-4000-8000-${String(id).padStart(12, '0')}`, title });
	const initial = track(1, 'Synthetic Prelude');
	const stale = track(2, 'Synthetic Interlude');
	const controlInitial = track(3, 'Synthetic Overture');
	const controlUpdated = track(4, 'Synthetic Finale');
	const refreshStarted = Promise.withResolvers<void>();
	const releaseRefresh = Promise.withResolvers<void>();

	async function wire(tab: Page, session: typeof invitee, first: typeof initial, updated: typeof initial) {
		let listings = 0;
		await tab.context().addCookies([{ name: fixtureCookie, value: session.cookie, url: origin, httpOnly: true }]);
		await tab.context().route('**/*', async (route) => {
			const request = route.request();
			const url = new URL(request.url());
			if (url.origin !== origin || request.method() !== 'GET') return route.abort();
			if (!url.search && (url.pathname === library.api || url.pathname === verifier)) {
				if (!hasFixtureSession(await request.headerValue('cookie'), session.cookie) || !sessions.has(session.cookie)) {
					return route.fulfill({ status: 401 });
				}
				if (url.pathname === verifier) return route.fulfill({ json: { session_binding: session.binding } });
				const listing = ++listings;
				if (session === invitee && listing === 2) {
					// This listing was admitted before revocation. Hold its successful reply across it.
					refreshStarted.resolve();
					await releaseRefresh.promise;
				}
				return route.fulfill({
					headers: { 'x-nntune-session-binding': session.binding }, json: [listing === 1 ? first : updated]
				});
			}
			if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/v1/')) return route.fulfill({ status: 404 });
			return route.continue();
		});
	}

	async function expectTrack(tab: Page, expected: typeof initial) {
		const rows = tab.getByTestId(library.item);
		await expect(rows).toHaveCount(1);
		await expect(rows).toHaveAttribute(library.id, expected.id);
		await expect(rows.getByRole('button', { name: expected.title, exact: true })).toBeVisible();
	}

	const controlContext = await browser.newContext({ serviceWorkers: 'block' });
	try {
		const control = await controlContext.newPage();
		await wire(page, invitee, initial, stale);
		await wire(control, active, controlInitial, controlUpdated);
		await Promise.all([page.goto(libraryUrl), control.goto(libraryUrl)]);
		await Promise.all([expectTrack(page, initial), expectTrack(control, controlInitial)]);
		const documentTimeOrigin = await page.evaluate(() => performance.timeOrigin);
		const staleRowObserved = await page.evaluateHandle(({ id, testId, trackId }) => {
			const selector = `[data-testid="${testId}"][${id}="${trackId}"]`;
			const evidence = { rendered: false };
			const observer = new MutationObserver((entries) => {
				if (entries.some(({ addedNodes }) => [...addedNodes].some((node) =>
					node instanceof Element && (node.matches(selector) || node.querySelector(selector))))) {
					evidence.rendered = true;
					observer.disconnect();
				}
			});
			observer.observe(document, { childList: true, subtree: true });
			return evidence;
		}, { id: library.id, testId: library.item, trackId: stale.id });

		await page.getByRole('button', { name: library.refresh, exact: true }).click();
		await refreshStarted.promise;
		await expect(page.getByRole('status').filter({ hasText: 'Loading your library…' })).toBeVisible();
		// Revoke the synthetic server session while the browser still holds its cookie.
		expect(sessions.delete(invitee.cookie)).toBe(true);
		const listing = page.waitForResponse(listingUrl);
		const verification = page.waitForResponse(origin + verifier);
		releaseRefresh.resolve();
		expect((await listing).status()).toBe(200);
		const refused = await verification;
		expect(refused.status()).toBe(401);
		expect(hasFixtureSession(await refused.request().headerValue('cookie'), invitee.cookie)).toBe(true);
		await expect(page.getByRole('alert')).toHaveText('Your session changed. Sign in again to see this library.');
		await expect(page.getByRole('link', { name: 'Sign in', exact: true })).toHaveAttribute('href', '/login');
		await expect(page.getByTestId(library.item)).toHaveCount(0);
		await expect(page).toHaveURL(libraryUrl);
		expect(await page.evaluate(() => performance.timeOrigin)).toBe(documentTimeOrigin);

		await control.getByRole('button', { name: library.refresh, exact: true }).click();
		await expectTrack(control, controlUpdated);
		await expect(page.getByTestId(library.item)).toHaveCount(0);
		expect(await staleRowObserved.evaluate(({ rendered }) => rendered)).toBe(false);
	} finally {
		releaseRefresh.resolve();
		await controlContext.close();
	}
});
