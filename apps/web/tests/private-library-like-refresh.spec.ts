import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

test('the invited Library preserves a Like confirmed during an older refresh', async ({ page, context, origin }) => {
	const { library } = PRIVATE_MUSIC;
	const listener = 'synthetic-like-listener';
	const binding = 'A'.repeat(43);
	const id = '00000000-0000-4000-8000-000000000001';
	const titles = ['Harbor Dawn', 'Harbor Morning', 'Harbor Daylight'];
	const likesPath = '/v1/me/likes';
	const mutationPath = `${likesPath}/track/${id}`;
	const preference = { kind: 'track', target_id: id };
	const createdAt = 1_700_000_000;
	const refreshStarted = Promise.withResolvers<void>();
	const releaseRefresh = Promise.withResolvers<void>();
	const likesReadStarted = Promise.withResolvers<void>();
	const tasteReadStarted = Promise.withResolvers<void>();
	const releaseLikesRead = Promise.withResolvers<void>();
	let listings = 0, reads = 0, tasteReads = 0, writes = 0;
	let serverLiked = false;

	await context.route('**/*', async (route) => {
		const request = route.request();
		const { origin: requestOrigin, pathname, search } = new URL(request.url());
		const method = request.method();
		if (requestOrigin !== origin || request.resourceType() === 'media') return route.abort();
		if (pathname.startsWith('/api/') || pathname.startsWith('/v1/')) {
			if (search || !hasFixtureSession(await request.headerValue('cookie'), listener)) {
				return route.fulfill({ status: 401 });
			}
			if (pathname === mutationPath && method === 'PUT') {
				expect(await request.headerValue('x-nntune-session-binding')).toBe(binding);
				writes += 1;
				serverLiked = true;
				return route.fulfill({ json: { target_id: id, liked_at: createdAt } });
			}
			if (method !== 'GET') return route.abort();
			if (pathname === library.api) {
				const title = titles[listings++];
				if (!title) return route.fulfill({ status: 500 });
				if (listings === 2) {
					refreshStarted.resolve();
					await releaseRefresh.promise;
				}
				return route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: [{ id, title, like_target_id: id }] });
			}
			if (pathname === `${likesPath}/session`) return route.fulfill({ json: { session_binding: binding } });
			if (pathname === likesPath) {
				const items = serverLiked ? [{ ...preference, created_at: createdAt }] : [];
				if (++reads === 2) {
					likesReadStarted.resolve();
					await releaseLikesRead.promise;
				}
				return route.fulfill({ json: { session_binding: binding, items } });
			}
			if (pathname === '/v1/me/taste/session') {
			const items = serverLiked ? [{ ...preference, signal: 'love', until: null }] : [];
			if (++tasteReads === 2) tasteReadStarted.resolve();
			return route.fulfill({ json: { session_binding: binding, items } });
			}
			return route.fulfill({ status: 404 });
		}
		return method === 'GET' ? route.continue() : route.abort();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
	await page.goto(origin + library.path);
	const row = page.getByTestId(library.item).and(page.locator(`[${library.id}="${id}"]`));
	const like = (title: string) => row.getByRole('button', { name: `Like ${title}`, exact: true });
	const refresh = page.getByRole('button', { name: library.refresh, exact: true });
	const loading = page.getByRole('status').filter({ hasText: 'Loading your library…' });
	await expect(like(titles[0])).toHaveAttribute('aria-pressed', 'false');

	try {
		await refresh.click();
		await refreshStarted.promise;
		await expect(loading).toBeVisible();
		await expect(like(titles[0])).toBeEnabled();
		await Promise.all([likesReadStarted.promise, tasteReadStarted.promise]);
		await like(titles[0]).click();
		await expect(like(titles[0])).toHaveAttribute('aria-pressed', 'true');
		await expect(like(titles[0])).toHaveAttribute('aria-disabled', 'false');
		await expect(loading).toBeVisible();
		expect(writes).toBe(1);
	} finally {
		releaseRefresh.resolve();
		releaseLikesRead.resolve();
	}

	// A new title on the same ID proves the older refresh actually published.
	await expect(like(titles[1])).toHaveAttribute('aria-pressed', 'true');
	await expect(loading).toBeHidden();
	expect(reads).toBe(2);

	// A later refresh must show a subsequent change made elsewhere.
	serverLiked = false;
	await refresh.click();
	await expect(like(titles[2])).toHaveAttribute('aria-pressed', 'false');
	await expect(loading).toBeHidden();
	expect([listings, reads, tasteReads, writes]).toEqual([3, 3, 3, 1]);
});
