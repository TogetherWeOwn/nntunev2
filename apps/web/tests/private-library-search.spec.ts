import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('the invited Library finds matching titles and clears back to the admitted tracks', async ({
	page,
	context,
	origin
}) => {
	const { library } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const verifierPath = '/v1/me/likes/session';
	const echo = { id: '00000000-0000-4000-8000-000000000001', title: 'Harbor Echo' };
	const lights = { id: '00000000-0000-4000-8000-000000000002', title: 'Harbor Lights' };
	const morning = { id: '00000000-0000-4000-8000-000000000003', title: 'Morning Echo' };
	const tracks = [echo, lights, morning];

	await context.route('**/*', async (route) => {
		const request = route.request();
		const { origin: requestOrigin, pathname } = new URL(request.url());
		if (requestOrigin !== origin || request.method() !== 'GET') return route.abort();
		if (pathname === library.api || pathname === verifierPath) {
			if (!hasFixtureSession(await request.headerValue('cookie'), listener)) {
				return route.fulfill({ status: 401 });
			}
			return pathname === library.api
				? route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: tracks })
				: route.fulfill({ json: { session_binding: binding } });
		}
		if (pathname.startsWith('/api/') || pathname.startsWith('/v1/')) {
			return route.fulfill({ status: 404 });
		}
		return route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);

	await page.goto(origin + library.path);
	const rows = page.getByTestId(library.item);
	const expectTracks = async (...expected: typeof tracks) => {
		await expect(rows).toHaveCount(expected.length);
		for (const { id, title } of expected) {
			const row = rows.and(page.locator(`[${library.id}="${id}"]`));
			await expect(row.getByRole('button', { name: title, exact: true })).toBeVisible();
		}
	};
	const find = page.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true });
	await expectTracks(...tracks);
	await find.fill('harbor');
	await expectTracks(echo, lights);
	await find.fill('harbor echo');
	await expectTracks(echo);

	await page.getByRole('button', { name: 'Clear search', exact: true }).click();
	await expect(find).toHaveValue('');
	await expectTracks(...tracks);
});
