import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, test } from './helpers/private-music-fixture.js';

const { library, player } = PRIVATE_MUSIC;
const verifierPath = '/v1/me/likes/session';
const listener = 'synthetic-listener';
const binding = 'A'.repeat(43);
const title = 'Morning Ferry';
const selected = { id: '00000000-0000-4000-8000-000000000001', title, artist: 'Alder' };
const peer = { id: '00000000-0000-4000-8000-000000000002', title, artist: 'Maple' };
const updated = { ...selected, artist: 'Alder Ensemble' };

for (const { name, retainSelection } of [
	{ name: 'Refresh clears a vanished selection while its same-title peer remains', retainSelection: false },
	{ name: 'Refresh preserves the selected ID beside its same-title peer', retainSelection: true }
]) {
	test(name, async ({ page, context, origin }) => {
		const snapshots = [[selected, peer], retainSelection ? [updated, peer] : [peer]];
		let listingReads = 0;
		await context.route('**/*', async (route) => {
			const request = route.request();
			const url = new URL(request.url());
			if (url.origin !== origin || request.method() !== 'GET') return route.abort();
			if (!url.search && (url.pathname === library.api || url.pathname === verifierPath)) {
				if (!hasFixtureSession(await request.headerValue('cookie'), listener)) {
					return route.fulfill({ status: 401 });
				}
				if (url.pathname === verifierPath) return route.fulfill({ json: { session_binding: binding } });
				const listing = snapshots[listingReads++];
				if (!listing) return route.fulfill({ status: 404 });
				return route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: listing });
			}
			if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/v1/')) {
				return route.fulfill({ status: 404 });
			}
			return route.continue();
		});
		await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);

		const rows = page.getByTestId(library.item);
		const row = (track: typeof selected) => rows.and(page.locator(`[${library.id}="${track.id}"]`));
		const titleButton = (track: typeof selected) =>
			row(track).getByRole('button', { name: `${track.title} — ${track.artist}`, exact: true });
		const checkbox = (track: typeof selected) =>
			row(track).getByRole('checkbox', { name: `Select ${track.title}`, exact: true });
		const selectionCount = page.getByRole('status').and(page.getByTestId('selection-count'));
		const append = page.getByRole('button', { name: 'Append selected', exact: true });
		const refresh = page.getByRole('button', { name: library.refresh, exact: true });
		const queue = page.getByTestId(player.queueItem);

		await page.goto(origin + library.path);
		await expect(rows).toHaveCount(2);
		await expect(titleButton(selected)).toBeVisible();
		await expect(titleButton(peer)).toBeVisible();
		await checkbox(selected).check();
		await expect(selectionCount).toHaveText('1 selected');
		await expect(checkbox(peer)).not.toBeChecked();
		await expect(append).toBeEnabled();
		await expect(queue).toHaveCount(0);
		expect(listingReads).toBe(1);

		const [response] = await Promise.all([
			page.waitForResponse(origin + library.api),
			refresh.click()
		]);
		expect(response.status()).toBe(200);
		await expect(rows).toHaveCount(retainSelection ? 2 : 1);
		await expect(titleButton(peer)).toBeVisible();
		await expect(checkbox(peer)).not.toBeChecked();
		await expect(queue).toHaveCount(0);

		if (retainSelection) {
			await expect(titleButton(updated)).toBeVisible();
			await expect(checkbox(updated)).toBeChecked();
		} else {
			await expect(row(selected)).toHaveCount(0);
			await expect(selectionCount).toHaveText('0 selected');
			await expect(append).toBeVisible();
			await expect(append).toBeDisabled();

			// The peer becomes an append target only after an explicit check.
			await checkbox(peer).check();
		}

		await expect(selectionCount).toHaveText('1 selected');
		await append.click();
		await expect(queue).toHaveCount(1);
		await expect(queue).toHaveAttribute(player.queueId, retainSelection ? selected.id : peer.id);
		expect(listingReads).toBe(2);
	});
}
