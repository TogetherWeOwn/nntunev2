import { expect, type Page } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, privateListeningRefusal, test } from './helpers/private-music-fixture.js';

test('a revoked invitee cannot reopen Library directly while a valid invitee can', async ({
	page, browser, origin
}) => {
	const { library } = PRIVATE_MUSIC;
	const libraryUrl = origin + library.path;
	const listingUrl = origin + library.api;
	const observedRow = 'data-observed-library-row';
	const first = {
		cookie: 'synthetic-first-invitee', binding: 'A'.repeat(43),
		track: { id: '00000000-0000-4000-8000-000000000001', title: 'Synthetic Prelude', duration_ms: 14_000 }
	};
	const valid = {
		cookie: 'synthetic-valid-invitee', binding: 'E'.repeat(43),
		track: { id: '00000000-0000-4000-8000-000000000002', title: 'Synthetic Finale', duration_ms: 14_000 }
	};
	let revoked = false;
	const denial = Promise.withResolvers<void>();

	async function wireInvitee(tab: Page, invitee: typeof first) {
		const context = tab.context();
		await tab.addInitScript(({ item, marker }) => {
			const selector = `[data-testid="${item}"]`;
			// Keep evidence of a row that appears and disappears between assertions.
			const observer = new MutationObserver((entries) => {
				for (const { addedNodes, removedNodes } of entries) {
					if ([...addedNodes, ...removedNodes].some((node) =>
						node instanceof Element && (node.matches(selector) || node.querySelector(selector)))) {
						document.documentElement.setAttribute(marker, 'true');
						observer.disconnect();
						return;
					}
				}
			});
			observer.observe(document, { childList: true, subtree: true });
		}, { item: library.item, marker: observedRow });
		await context.addCookies([{
			name: fixtureCookie, value: invitee.cookie, url: origin, httpOnly: true, sameSite: 'Lax'
		}]);
		await context.route('**/*', async (route) => {
			const request = route.request();
			const url = new URL(request.url());
			if (url.origin !== origin || request.method() !== 'GET') return route.abort();
			const verifiesSession = url.pathname === '/v1/me/likes/session';
			if (url.pathname === library.api || verifiesSession) {
				if (!hasFixtureSession(await request.headerValue('cookie'), invitee.cookie)) {
					return route.fulfill({ status: 401 });
				}
				// Account authentication is separate from private listening admission.
				if (verifiesSession) return route.fulfill({ json: { session_binding: invitee.binding } });
				if (invitee === first && revoked) {
					await denial.promise;
					return route.fulfill({ status: 403, json: privateListeningRefusal });
				}
				return route.fulfill({
					headers: { 'x-nntune-session-binding': invitee.binding }, json: [invitee.track]
				});
			}
			if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/v1/')) return route.fulfill({ status: 404 });
			return route.continue();
		});
	}

	async function openLibrary(tab: Page) {
		const document = await tab.goto(libraryUrl);
		expect(document?.status()).toBe(200);
		expect(document?.request().resourceType()).toBe('document');
	}

	async function expectAdmitted(tab: Page, invitee: typeof first) {
		const listed = tab.waitForResponse(listingUrl);
		await openLibrary(tab);
		expect((await listed).status()).toBe(200);
		const rows = tab.getByTestId(library.item);
		await expect(rows).toHaveCount(1);
		await expect(rows).toHaveAttribute(library.id, invitee.track.id);
		await expect(rows.getByRole('button', { name: invitee.track.title, exact: true })).toBeVisible();
		await expect(tab.locator('html')).toHaveAttribute(observedRow, 'true');
		await expect(tab).toHaveURL(libraryUrl);
	}

	const readSnapshot = () => page.evaluate(() => {
		const context = sessionStorage.getItem('nntune.library.auth-context.v1');
		return context ? sessionStorage.getItem(`nntune.library.snapshot.v1.${context}`) : null;
	});
	await wireInvitee(page, first);
	await expectAdmitted(page, first);
	const snapshot = await readSnapshot();
	expect(snapshot).toContain(first.track.id);

	// Keep the cookie and same-tab storage, then start from a blank document.
	await page.goto('about:blank');
	revoked = true;
	const listingRequested = page.waitForRequest(listingUrl);
	const refused = page.waitForResponse(listingUrl);
	try {
		await openLibrary(page);
		await listingRequested;
		await expect(page.getByRole('status').filter({ hasText: 'Loading your library…' })).toBeVisible();
		expect(await readSnapshot()).toBe(snapshot);
		await expect(page.getByTestId(library.item)).toHaveCount(0);
	} finally {
		denial.resolve();
	}
	expect((await refused).status()).toBe(403);
	await expect(page.getByRole('alert')).toHaveText(library.forbidden);
	await expect(page.getByRole('button', { name: 'Retry', exact: true })).toBeVisible();
	await expect(page.getByRole('link', { name: 'Sign in', exact: true })).toHaveCount(0);
	await expect(page.getByTestId(library.item)).toHaveCount(0);
	await expect(page.getByText(first.track.title, { exact: true })).toHaveCount(0);
	expect(await page.locator('html').getAttribute(observedRow), 'a Library row rendered in the denied document').toBeNull();
	await expect(page).toHaveURL(libraryUrl);

	const validContext = await browser.newContext({ serviceWorkers: 'block' });
	try {
		const control = await validContext.newPage();
		await wireInvitee(control, valid);
		await expectAdmitted(control, valid);
	} finally {
		await validContext.close();
	}
});
