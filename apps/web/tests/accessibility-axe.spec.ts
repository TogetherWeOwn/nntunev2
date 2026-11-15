import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page } from '@playwright/test';

/**
 * Axe accessibility gate: every existing route must report zero serious or
 * critical axe violations. The full default axe ruleset runs; lower-severity
 * (minor/moderate) findings are attached to the test report as warnings but
 * do not fail the gate. Known moderate best-practice findings at introduction:
 * the layout skip link targets `#main-content`, which no page renders yet
 * (`skip-link`), and the skip link plus appearance control sit outside a
 * landmark (`region`). Visual-regression screenshot baselines are deferred
 * until the next shell rebuild lands; this file asserts semantics only.
 *
 * Network is mocked the same way as the other shell specs: API routes fail
 * closed with 503 so pages render their signed-out / empty states.
 */
const ROUTES = [
	'/',
	'/login',
	'/signup',
	'/pricing',
	'/auth/magic',
	'/account/security',
	'/account/billing',
	'/home',
	'/search',
	'/settings',
	'/library',
	'/library/liked',
	'/library/recent'
] as const;

const MOBILE_ROUTES = new Set<string>(['/library']);

async function mockApi(page: Page, route: string) {
	await page.route('**/v1/**', (r) =>
		r.fulfill({ status: 503, contentType: 'application/json', body: '{}' })
	);
	// The library renders its row controls only with tracks present; scan that
	// populated state instead of the empty list. Search and Liked render
	// their result rows and remove controls only from the same rows, so
	// they scan populated too.
	const library =
		route === '/library' || route === '/search' || route === '/library/liked'
			? [
					{ id: 'mango', title: 'Mango Season', artist: 'Acceptance Band' },
					{ id: 'apple', title: 'Apple Cart', artist: 'Acceptance Band' }
				]
			: [];
	await page.route('**/api/library', (r) =>
		r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(library) })
	);
}

async function expectNoSeriousOrCriticalViolations(page: Page, route: string) {
	const results = await new AxeBuilder({ page }).analyze();
	const blocking = results.violations.filter(
		(violation) => violation.impact === 'serious' || violation.impact === 'critical'
	);
	const summary = blocking
		.map(
			(violation) =>
				`[${violation.impact}] ${violation.id}: ${violation.help}\n` +
				violation.nodes.map((node) => `  ${node.target.join(' ')}`).join('\n')
		)
		.join('\n');
	expect(blocking, `${route} has serious/critical axe violations:\n${summary}`).toEqual([]);

	const warnings = results.violations.filter(
		(violation) => violation.impact !== 'serious' && violation.impact !== 'critical'
	);
	if (warnings.length > 0) {
		await test.info().attach('axe warnings (minor/moderate)', {
			body: JSON.stringify(warnings, null, 2),
			contentType: 'application/json'
		});
	}
}

async function visit(page: Page, route: string) {
	await mockApi(page, route);
	// Search renders results only for an applied query; drive the populated
	// state through the URL the top-bar search and the page share (?q=).
	const url = route === '/search' ? '/search?q=mango' : route;
	if (route === '/library/liked') {
		// Liked projects stored favorites onto the library rows; seed a
		// fixed auth context so the remove controls render populated.
		await page.addInitScript(() => {
			sessionStorage.setItem('nntune.library.auth-context.v1', 'axe-populated');
			localStorage.setItem(
				'nntune.library.favorites.v1.axe-populated',
				JSON.stringify(['mango', 'apple'])
			);
		});
	}
	await page.goto(url, { waitUntil: 'load' });
	await expect(page.locator('main')).toBeVisible();
	if (route === '/library') {
		await expect(page.getByTestId('library-item').first()).toBeVisible();
	}
	if (route === '/search') {
		await expect(page.getByText(/results? for/)).toBeVisible();
	}
	if (route === '/library/liked') {
		await expect(page.getByRole('button', { name: /from Liked/ }).first()).toBeVisible();
	}
	await page.waitForLoadState('networkidle', { timeout: 5_000 }).catch(() => {});
}

for (const route of ROUTES) {
	test(`axe: ${route} has no serious or critical violations`, async ({ page }) => {
		await visit(page, route);
		await expectNoSeriousOrCriticalViolations(page, route);
	});

	if (MOBILE_ROUTES.has(route)) {
		test(`axe: ${route} has no serious or critical violations at 390px`, async ({ page }) => {
			await page.setViewportSize({ width: 390, height: 844 });
			await visit(page, route);
			await expectNoSeriousOrCriticalViolations(page, `${route} (390px)`);
		});
	}
}
