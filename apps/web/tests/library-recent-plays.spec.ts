import { expect, test } from '@playwright/test';

const plays = [
	{ play_id: 'Play_B', track_id: 'Job_A', played_at: 1_700_000_002, stream_seconds: 10 },
	{ play_id: 'Play_A', track_id: 'Job_A', played_at: 1_700_000_000, stream_seconds: 4 },
	{ play_id: 'Play_C', track_id: 'Job_Missing', played_at: 1_699_999_999, stream_seconds: 0 }
];

const recording = '00000000-0000-4000-8000-000000000001';
const unplayed = '00000000-0000-4000-8000-000000000002';
const catalogue = [
	{ id: recording, occurrence_key: 'Job_A:1', recording_mbids: [recording], title: 'Coral Horizon' },
	{ id: unplayed, occurrence_key: 'Job_Unplayed:1', recording_mbids: [unplayed], title: 'Unplayed Title' }
];

test('Recent lists only metered plays and clears them on an auth transition', async ({ page }) => {
	await page.route('**/v1/me/history', (route) => route.fulfill({ json: { items: plays } }));
	await page.route('**/v1/library', (route) => route.fulfill({ json: catalogue }));
	await page.goto('/library/recent');

	const history = page.getByRole('region', { name: 'Recently played' });
	const entries = history.getByRole('listitem');
	await expect(entries).toHaveText([/Coral Horizon/, /Coral Horizon/, /Unknown track/]);
	await expect(entries.first().locator('time')).toHaveAttribute('datetime', '2023-11-14T22:13:22.000Z');
	await expect(page.getByRole('region', { name: 'Recent searches' })).toBeVisible();

	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', {
		key: 'nntune.library.auth-transition.v1'
	})));
	await expect(history.getByRole('link', { name: 'Sign in' })).toBeVisible();
	await expect(entries).toHaveCount(0);
});

test('Recent stays signed out when pending catalogue enrichment completes', async ({ page }) => {
	await page.addInitScript((rows) => {
		const fetch = window.fetch.bind(window);
		window.fetch = (input, init) => {
			if (typeof input !== 'string' || !input.endsWith('/v1/library')) return fetch(input, init);
			// Hold a successful response even if the caller aborts in the meantime.
			return new Promise<Response>((resolve) => {
				window.addEventListener('release-recent-catalogue', () => resolve(Response.json(rows)), { once: true });
				document.documentElement.dataset.recentCataloguePending = 'true';
			});
		};
	}, catalogue);
	await page.route('**/v1/me/history', (route) => route.fulfill({ json: { items: plays } }));
	await page.goto('/library/recent');

	const history = page.getByRole('region', { name: 'Recently played' });
	const signIn = history.getByRole('link', { name: 'Sign in' });
	await expect(page.locator('html')).toHaveAttribute('data-recent-catalogue-pending', 'true');
	await expect(history.getByRole('status')).toContainText('Loading');
	await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', {
		key: 'nntune.library.auth-transition.v1'
	})));
	await expect(signIn).toBeVisible();

	await page.evaluate(() => new Promise<void>((resolve) => {
		window.dispatchEvent(new Event('release-recent-catalogue'));
		// The response body is in memory. Let its promise chain and Svelte's
		// render microtasks drain before the next browser task, not a timed wait.
		const channel = new MessageChannel();
		channel.port1.onmessage = () => {
			channel.port1.close();
			channel.port2.close();
			resolve();
		};
		channel.port2.postMessage(null);
	}));
	await expect(signIn).toBeVisible();
	await expect(history.getByRole('listitem')).toHaveCount(0);
});

test('Recent offers sign-in when the initial history request returns 401', async ({ page }) => {
	await page.route('**/v1/me/history', (route) => route.fulfill({ status: 401, json: {} }));
	await page.goto('/library/recent');

	const history = page.getByRole('region', { name: 'Recently played' });
	await expect(history.getByRole('link', { name: 'Sign in' })).toBeVisible();
	await expect(history.getByRole('listitem')).toHaveCount(0);
});

test('Recent offers retry after a read failure and distinguishes an empty result', async ({ page }) => {
	let requests = 0;
	await page.route('**/v1/me/history', (route) => ++requests === 1
		? route.fulfill({ status: 503, json: {} })
		: route.fulfill({ json: { items: [] } }));
	await page.goto('/library/recent');

	const history = page.getByRole('region', { name: 'Recently played' });
	await expect(history.getByRole('alert')).toContainText('Could not load');
	await history.getByRole('button', { name: 'Retry' }).click();
	await expect(history.getByText('No recent plays yet.')).toBeVisible();
	await expect(history.getByRole('listitem')).toHaveCount(0);
});

test('Recent refuses a pending history response from the previous account', async ({ page }) => {
	await page.addInitScript((items) => {
		const original = window.fetch.bind(window);
		let first = true;
		window.fetch = (input, init) => {
			if (typeof input !== 'string' || !input.endsWith('/v1/me/history') || !first) return original(input, init);
			first = false;
			const dataset = document.documentElement.dataset;
			dataset.historyPending = 'true';
			init?.signal?.addEventListener('abort', () => (dataset.historyAborted = 'true'), { once: true });
			return new Promise<Response>((resolve) => {
				window.addEventListener('release-old-history', () => resolve(Response.json({ items })), { once: true });
			});
		};
	}, plays);
	await page.route('**/v1/me/history', (route) => route.fulfill({ json: { items: [] } }));
	await page.route('**/v1/library', (route) => route.fulfill({ json: catalogue }));
	await page.goto('/library/recent');
	const history = page.getByRole('region', { name: 'Recently played' });
	await expect(page.locator('html')).toHaveAttribute('data-history-pending', 'true');
	await page.evaluate(() => {
		const key = 'nntune.auth.generation.v1';
		localStorage.setItem(key, 'replacement-session');
		window.dispatchEvent(new StorageEvent('storage', { key }));
	});
	await expect(page.locator('html')).toHaveAttribute('data-history-aborted', 'true');
	await history.getByRole('button', { name: 'Retry' }).click();
	await expect(history.getByText('No recent plays yet.')).toBeVisible();
	await page.evaluate(() => window.dispatchEvent(new Event('release-old-history')));
	await expect(history.getByText('No recent plays yet.')).toBeVisible();
	await expect(history.getByRole('listitem')).toHaveCount(0);
});
