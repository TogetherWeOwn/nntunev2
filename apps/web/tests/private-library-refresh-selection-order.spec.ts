import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

for (const reorder of [true, false]) {
	test(`the invited Library keeps the selected track when Refresh ${reorder ? 'swaps' : 'keeps'} row order`, async ({
		page, context, origin
	}) => {
		const { library, player } = PRIVATE_MUSIC;
		const listener = 'synthetic-refresh-listener';
		const binding = 'A'.repeat(43);
		const verifierPath = '/v1/me/likes/session';
		const neighbor = { id: '00000000-0000-4000-8000-000000000001', title: 'Amber Tone' };
		const target = { id: '00000000-0000-4000-8000-000000000002', title: 'Dusk Tone' };
		const initial = [neighbor, target];
		const fresh = reorder ? [target, neighbor] : initial;
		const unexpectedRequests: string[] = [];
		let listingReads = 0;
		let releaseRefresh!: () => void;
		const refreshPending = new Promise<void>((resolve) => { releaseRefresh = resolve; });

		await context.route('**/*', async (route) => {
			const request = route.request();
			const { origin: requestOrigin, pathname, search } = new URL(request.url());
			if (requestOrigin !== origin || request.method() !== 'GET' || request.resourceType() === 'media' ||
				pathname === player.plays || pathname.startsWith(`${player.plays}/`)) {
				unexpectedRequests.push(`${request.method()} ${pathname}`);
				return route.abort();
			}
			if (!search && (pathname === library.api || pathname === verifierPath)) {
				if (!hasFixtureSession(await request.headerValue('cookie'), listener)) return route.fulfill({ status: 401 });
				if (pathname === verifierPath) return route.fulfill({ json: { session_binding: binding } });
				const isRefresh = ++listingReads > 1;
				if (isRefresh) await refreshPending;
				return route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: isRefresh ? fresh : initial });
			}
			if (pathname.startsWith('/api/') || pathname.startsWith('/v1/')) return route.fulfill({ status: 404 });
			return route.continue();
		});
		await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);

		const rows = page.getByTestId('library-list').getByTestId(library.item);
		const row = (id: string) => rows.and(page.locator(`[${library.id}="${id}"]`));
		const rowIds = () => rows.evaluateAll((nodes, attribute) => nodes.map((node) => node.getAttribute(attribute)), library.id);
		const checkbox = (track: typeof target) => row(track.id).getByRole('checkbox', { name: `Select ${track.title}`, exact: true });
		const selectionCount = page.getByTestId('selection-count');
		const append = page.getByRole('button', { name: 'Append selected', exact: true });
		const queue = page.getByTestId(player.queueItem);
		const current = page.locator(player.currentQueueItem);
		const refresh = page.getByRole('button', { name: library.refresh, exact: true });
		const loading = page.getByRole('status').filter({ hasText: 'Loading your library…' });

		await page.goto(origin + library.path);
		await expect.poll(rowIds).toEqual([neighbor.id, target.id]);
		await expect(selectionCount).toHaveText('0 selected');
		await expect(queue).toHaveCount(0);
		await checkbox(target).check();
		await expect(checkbox(target)).toBeChecked();
		await expect(checkbox(neighbor)).not.toBeChecked();
		await expect(selectionCount).toHaveText('1 selected');
		await expect(append).toBeEnabled();
		await row(target.id).getByRole('button', { name: target.title, exact: true }).click();
		await expect(current).toHaveAttribute(player.queueId, target.id);
		expect(listingReads).toBe(1);

		const [response] = await Promise.all([
			page.waitForResponse(origin + library.api),
			(async () => {
				try {
					await refresh.press('Enter');
					await expect(loading).toBeVisible();
				} finally {
					releaseRefresh();
				}
			})()
		]);
		expect(response.status()).toBe(200);
		await expect(loading).toBeHidden();
		await expect.poll(rowIds).toEqual(fresh.map(({ id }) => id));
		await expect(refresh).toBeFocused();
		await expect(checkbox(target)).toBeChecked();
		await expect(checkbox(neighbor)).not.toBeChecked();
		await expect(selectionCount).toHaveText('1 selected');
		await expect(current).toHaveAttribute(player.queueId, target.id);

		await append.click();
		await expect(page.getByTestId('append-result')).toHaveText('Added 0 tracks (1 already queued)');
		await expect(queue).toHaveCount(1);
		await expect(current).toHaveAttribute(player.queueId, target.id);
		expect(listingReads).toBe(2);
		expect(unexpectedRequests).toEqual([]);
	});
}
