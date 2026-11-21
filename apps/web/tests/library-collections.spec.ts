import { expect, test, type Page } from '@playwright/test';

const mbid = (i: number) => `11111111-1111-4111-8111-${String(i).padStart(12, '0')}`;
const artPath = `/v1/releases/${mbid(1)}/art`;
const cafe = { mbid: mbid(1), title: 'Café', artist: 'North', art_url: artPath, release_type: 'Album' };
const single = { mbid: mbid(2), title: 'Loose Track', release_type: 'Single' };
const tracks = ['Track One', 'Track Two'].map((title, i) => ({ id: 'track-a', occurrence_key: `job:${i + 1}`, title }));
const controls = (page: Page) => ({
	choose: (name: string) => page.getByRole('group', { name: 'Library type' }).getByRole('button', { name, exact: true }).click(),
	region: page.getByRole('region', { name: 'Library collections' })
});
const crossTabReset = (page: Page) => page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1' })));

test('browses member collections without changing track selection or queue', async ({ page }) => {
	await page.setViewportSize({ width: 360, height: 900 });
	await page.route('**/api/library', (route) => route.fulfill({ json: tracks }));
	await page.route('**/v1/releases', (route) => route.fulfill({ json: [{ ...cafe, mbid: mbid(3), title: 'Élan', art_url: null }, cafe, single,
		...Array.from({ length: 47 }, (_, i) => ({ ...single, mbid: mbid(i + 4), release_type: 'EP' }))] }));
	await page.route('**/v1/artists', (route) => route.fulfill({ json: Array.from({ length: 50 }, (_, i) => ({ mbid: mbid(i + 1), name: `Artist ${i + 1}` })) }));
	await page.route(`**${artPath}`, (route) => route.fulfill({ status: 404 }));
	await page.goto('/library');
	const trackRows = page.getByTestId('library-item'), queue = page.getByTestId('queue-item'), meter = page.getByTestId('usage-meter');
	await expect(trackRows).toHaveCount(2);
	await trackRows.first().getByRole('checkbox').check();
	await page.getByRole('button', { name: 'Append selected', exact: true }).click();
	await expect(queue).toHaveText(/Track One/);
	const queued = await queue.elementHandle();
	const { choose, region } = controls(page), rows = region.getByRole('listitem');
	await Promise.all([page.waitForResponse((response) => response.url().endsWith(artPath) && response.status() === 404), choose('All collections')]);
	await expect(rows).toHaveCount(52);
	await expect(trackRows.first()).toBeHidden();
	await expect(meter).toHaveCount(1);
	await choose('Artists');
	await expect(rows).toHaveText(Array.from({ length: 50 }, (_, i) => new RegExp(`Artist ${i + 1}\\s*Artist`)));
	await choose('Albums');
	await region.getByRole('combobox', { name: 'Sort collections' }).selectOption('title');
	await expect(rows).toHaveText([/Café/, /Élan/]);
	await region.getByRole('searchbox').fill('Café');
	await expect(rows).toHaveText([/Café/]);
	const row = await rows.elementHandle(), comfortable = (await rows.boundingBox())!.height;
	await page.getByRole('combobox', { name: 'Density' }).selectOption('compact');
	await expect.poll(async () => (await rows.boundingBox())!.height).toBeLessThan(comfortable);
	await region.getByRole('combobox', { name: 'Collection view' }).selectOption('grid');
	await expect(region.getByRole('list')).toHaveCSS('display', 'grid');
	await expect(rows.locator('img')).toHaveAttribute('src', '/placeholder-artwork.svg');
	expect(await rows.evaluate((node, original) => node === original, row)).toBe(true);
	await choose('Tracks');
	await expect(trackRows.first().getByRole('checkbox')).toBeChecked();
	await expect(queue).toHaveText(/Track One/);
	expect(await queue.evaluate((node, original) => node === original, queued)).toBe(true);
	await expect(meter).toHaveCount(1);
});

test('retries collection reads and renders single, empty and sign-in states', async ({ page }) => {
	await page.route('**/api/library', (route) => route.fulfill({ json: [] }));
	let requests = 0;
	await page.route('**/v1/releases', (route) => route.fulfill(++requests === 1 ? { status: 503, json: {} } : { json: [single] }));
	await page.route('**/v1/artists', (route) => route.fulfill({ json: [{ mbid: mbid(1), name: 'North' }] }));
	await page.goto('/library');
	const { choose, region } = controls(page);
	await choose('Albums');
	await expect(region.getByRole('alert')).toContainText('Could not load');
	await region.getByRole('button', { name: 'Retry collections' }).click();
	await expect(region.getByRole('status')).toHaveText('No albums in the loaded results.');
	await choose('Artists');
	await region.getByRole('searchbox').fill('No matching collection');
	await region.getByRole('button', { name: 'Clear collection search' }).press('Enter');
	await expect(region.getByRole('searchbox')).toBeFocused();
	await expect(region.getByRole('listitem')).toHaveText([/North/]);
	await page.route('**/v1/releases', (route) => route.fulfill({ status: 401, json: {} }));
	await choose('All collections');
	await expect(region.getByRole('link', { name: 'Sign in' })).toBeVisible();
});

for (const boundary of [401, 403, 'cross-tab', 'filter'] as const) {
	test(`withholds collection rows across ${boundary}`, async ({ page }) => {
		const tracksStatus = Promise.withResolvers<number>();
		await page.route('**/api/library', async (route) => route.fulfill({ status: typeof boundary === 'number' ? await tracksStatus.promise : 200, json: [] }));
		await page.route('**/v1/artists', (route) => route.fulfill({ json: [{ mbid: mbid(1), name: 'North' }] }));
		await page.goto('/library');
		const late = await page.evaluateHandle(() => {
			const result = Promise.withResolvers<Response>(), original = window.fetch;
			let signal: AbortSignal | null | undefined;
			window.fetch = (url, init) => {
				if (!String(url).endsWith('/v1/releases')) return original(url, init);
				signal = init?.signal;
				return result.promise;
			};
			return { pending: () => Boolean(signal), aborted: () => signal?.aborted, reply: (body: unknown) => result.resolve(Response.json(body)) };
		});
		const { choose, region } = controls(page);
		await choose('Albums');
		if (typeof boundary === 'number') {
			await expect(region).toHaveCount(0);
			tracksStatus.resolve(boundary);
			await expect(region.getByRole('link', { name: 'Sign in' })).toBeVisible();
			expect(await late.evaluate((read) => read.pending())).toBe(false);
			return;
		}
		await region.getByRole('searchbox').fill('North');
		await expect.poll(() => late.evaluate((read) => read.pending())).toBe(true);
		if (boundary === 'cross-tab') await crossTabReset(page);
		if (boundary === 'filter') await choose('Artists');
		await expect.poll(() => late.evaluate((read) => read.aborted())).toBe(true);
		await late.evaluate((read, album) => read.reply([album]), cafe);
		if (boundary === 'filter') {
			await expect(region.getByRole('listitem')).toHaveText([/North/]);
		} else {
			await expect(region.getByRole('link', { name: 'Sign in' })).toBeVisible();
			await expect(region.getByRole('searchbox')).toHaveValue('');
			await expect(region.getByRole('listitem')).toHaveCount(0);
		}
	});
}
