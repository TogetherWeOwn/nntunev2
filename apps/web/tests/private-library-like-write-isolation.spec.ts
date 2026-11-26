import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('the invited Library preserves a confirmed Like when another track write fails', async ({ page, context, origin }) => {
	const { library } = PRIVATE_MUSIC;
	const listener = 'synthetic-like-write-listener';
	const binding = 'A'.repeat(43);
	const likesPath = '/v1/me/likes';
	const createdAt = 1_700_000_000;
	const track = (suffix: string, title: string) => {
		const id = `00000000-0000-4000-8000-00000000000${suffix}`;
		return { id, title, like_target_id: id };
	};
	const refused = track('1', 'Harbor Dawn');
	const confirmed = track('2', 'Tidal Echo');
	const untouched = track('3', 'Coral Horizon');
	const initialLikes = [refused, untouched].map(({ id }) => ({
		kind: 'track', target_id: id, created_at: createdAt
	}));
	const releaseFailure = Promise.withResolvers<void>();
	const refusedPath = `${likesPath}/track/${refused.id}`;
	const confirmedPath = `${likesPath}/track/${confirmed.id}`;
	const writes: string[] = [];
	const unexpected: string[] = [];
	let listingReads = 0, likesReads = 0;

	await context.route('**/*', async (route) => {
		const request = route.request();
		const { origin: requestOrigin, pathname, search } = new URL(request.url());
		const method = request.method();
		const reject = () => {
			unexpected.push(`${method} ${pathname}`);
			return route.abort();
		};
		if (requestOrigin !== origin || request.resourceType() === 'media') return reject();
		if (!pathname.startsWith('/api/') && !pathname.startsWith('/v1/')) {
			return method === 'GET' ? route.continue() : reject();
		}
		if (search || !hasFixtureSession(await request.headerValue('cookie'), listener)) return reject();
		if (method === 'GET') {
			if (pathname === library.api) {
				listingReads++;
				return route.fulfill({
					headers: { 'x-nntune-session-binding': binding },
					json: [refused, confirmed, untouched]
				});
			}
			if (pathname === likesPath) {
				likesReads++;
				return route.fulfill({ json: { session_binding: binding, items: initialLikes } });
			}
			if (pathname === `${likesPath}/session`) return route.fulfill({ json: { session_binding: binding } });
			if (pathname === '/v1/me/taste/session') return route.fulfill({ json: { session_binding: binding, items: [] } });
			return route.fulfill({ status: 404 });
		}
		if ((pathname !== refusedPath || method !== 'DELETE') && (pathname !== confirmedPath || method !== 'PUT')) {
			return reject();
		}
		expect(await request.headerValue('x-nntune-session-binding')).toBe(binding);
		writes.push(`${method} ${pathname}`);
		if (pathname === refusedPath) {
			await releaseFailure.promise;
			return route.fulfill({ status: 503, json: {} });
		}
		return route.fulfill({ json: { target_id: confirmed.id, liked_at: createdAt } });
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
	await page.goto(origin + library.path);

	const button = ({ id, title }: typeof refused) =>
		page.getByTestId(library.item).and(page.locator(`[${library.id}="${id}"]`))
			.getByRole('button', { name: `Like ${title}`, exact: true });
	const first = button(refused), second = button(confirmed), third = button(untouched);
	const error = page.getByRole('status').filter({ hasText: 'Could not confirm that Like change. Try again or refresh the library.' });
	await expect(first).toHaveAttribute('aria-pressed', 'true');
	await expect(second).toHaveAttribute('aria-pressed', 'false');
	await expect(third).toHaveAttribute('aria-pressed', 'true');

	try {
		await first.click();
		await expect.poll(() => writes).toEqual([`DELETE ${refusedPath}`]);
		await Promise.all([page.waitForResponse(origin + confirmedPath), second.click()]);
		await expect(second).toHaveAttribute('aria-pressed', 'true');
		await expect(first).toHaveAttribute('aria-pressed', 'true');
		await expect(error).toHaveCount(0);
		const failed = page.waitForResponse(origin + refusedPath);
		releaseFailure.resolve();
		expect((await failed).status()).toBe(503);
	} finally {
		releaseFailure.resolve();
	}

	await expect(error).toBeVisible();
	await expect(first).toHaveAttribute('aria-pressed', 'true');
	await expect(second).toHaveAttribute('aria-pressed', 'true');
	await expect(third).toHaveAttribute('aria-pressed', 'true');
	await expect(page).toHaveURL(origin + library.path);
	expect(writes).toEqual([`DELETE ${refusedPath}`, `PUT ${confirmedPath}`]);
	expect([listingReads, likesReads]).toEqual([1, 1]);
	expect(unexpected).toEqual([]);
});
