import { expect, test } from '@playwright/test';
import { openQueue } from './helpers/queue-host';

const rows = [
	{ id: 'recording-a', occurrence_key: 'job-a:1', title: 'Echo One', artist: 'First Artist' },
	{ id: 'recording-a', occurrence_key: 'job-a:2', title: 'Echo Two', artist: 'Second Artist' },
	{ id: 'recording-a', occurrence_key: 'job-b:1', title: 'Echo Three', artist: 'Third Artist' }
];

test('selecting the second occurrence queues its metadata with the recording id', async ({ page }) => {
	await page.route('**/api/library', (route) => route.fulfill({ json: rows }));
	await page.goto('/library');
	await openQueue(page);
	const second = page.getByTestId('library-item').filter({ hasText: 'Echo Two' });
	await second.getByRole('button', { name: 'Echo Two — Second Artist', exact: true }).click();
	const queued = page.getByTestId('queue-item');
	await expect(queued).toHaveCount(1);
	await expect(queued).toHaveAttribute('data-track-id', 'recording-a');
	await expect(queued).toContainText('Echo Two');
	await expect(queued).toContainText('Second Artist');
	await expect(queued).not.toContainText('First Artist');
	await expect(page.getByTestId('now-playing')).toContainText('Echo Two');

	// Recording-based queue/favorite identity is unchanged by occurrence selection.
	await page.getByTestId('library-item').first().locator('.nt-library-row').click();
	await expect(queued).toHaveCount(1);
	await expect(queued).toContainText('Second Artist');
	await second.getByRole('button', { name: 'Star Echo Two', exact: true }).click();
	await expect(page.getByTestId('library-item').getByRole('button', { name: /^Unstar/ })).toHaveCount(3);
});

test('the second occurrence copy link names the row but links the recording', async ({ page }) => {
	await page.addInitScript(() => {
		Object.defineProperty(navigator, 'clipboard', {
			configurable: true,
			value: { writeText: async (text: string) => sessionStorage.setItem('copied-occurrence-link', text) }
		});
	});
	await page.route('**/api/library', (route) => route.fulfill({ json: rows }));
	await page.goto('/library');
	await openQueue(page);
	await page.getByTestId('library-item').filter({ hasText: 'Echo Two' }).getByTestId('library-copy-link').click();
	await expect(page.getByTestId('library-share-status')).toHaveText('Copied link for Echo Two.');
	const copied = await page.evaluate(() => sessionStorage.getItem('copied-occurrence-link'));
	expect(new URL(copied!).searchParams.get('track')).toBe('recording-a');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('dragging the second occurrence queues its own metadata', async ({ page }) => {
	await page.setViewportSize({ width: 1280, height: 1800 });
	await page.route('**/api/library', (route) => route.fulfill({ json: rows }));
	await page.goto('/library');
	await openQueue(page);
	await page.getByTestId('library-item').filter({ hasText: 'Echo Two' }).dragTo(
		page.getByTestId('queue-drop-target'), { sourcePosition: { x: 4, y: 4 } }
	);
	const queued = page.getByTestId('queue-item');
	await expect(queued).toHaveCount(1);
	await expect(queued).toHaveAttribute('data-track-id', 'recording-a');
	await expect(queued).toContainText('Echo Two');
	await expect(queued).toContainText('Second Artist');
	await expect(queued).not.toContainText('First Artist');
});

test('a selected recording menu counts once and retains the invoked occurrence', async ({ page }) => {
	await page.route('**/api/library', (route) => route.fulfill({ json: rows }));
	await page.goto('/library');
	await openQueue(page);
	const second = page.getByTestId('library-item').filter({ hasText: 'Echo Two' });
	await second.getByRole('checkbox', { name: 'Select Echo Two', exact: true }).check();
	await second.getByTestId('track-more').click();
	await expect(page.getByRole('menu')).toHaveAccessibleName('Actions for Echo Two');
	await expect(page.getByRole('menuitem', { name: 'Copy link', exact: true })).toBeVisible();
	await page.getByRole('menuitem', { name: 'Add to queue', exact: true }).click();
	const queued = page.getByTestId('queue-item');
	await expect(queued).toHaveCount(1);
	await expect(queued).toContainText('Second Artist');
	await expect(queued).not.toContainText('First Artist');
});

for (const action of ['Play now', 'Play next', 'Add to queue']) {
	test(`second occurrence ${action} keeps its own queue metadata`, async ({ page }) => {
		await page.route('**/api/library', (route) => route.fulfill({ json: rows }));
		await page.goto('/library');
		await openQueue(page);
		const second = page.getByTestId('library-item').filter({ hasText: 'Echo Two' });
		await second.getByTestId('track-more').click();
		await expect(page.getByRole('menu')).toHaveAccessibleName('Actions for Echo Two');
		await page.getByRole('menuitem', { name: action, exact: true }).click();
		const queued = page.getByTestId('queue-item');
		await expect(queued).toHaveCount(1);
		await expect(queued).toHaveAttribute('data-track-id', 'recording-a');
		await expect(queued).toContainText('Echo Two');
		await expect(queued).toContainText('Second Artist');
		await expect(queued).not.toContainText('First Artist');
	});
}

for (const path of ['/library', '/home', '/search?q=Echo', '/library/liked']) {
	test(`catalogue occurrences render without duplicate keys on ${path}`, async ({ page }) => {
		const errors: Error[] = [];
		page.on('pageerror', (error) => errors.push(error));
		await page.addInitScript(() => {
			const context = 'occurrence-test-context';
			sessionStorage.setItem('nntune.library.auth-context.v1', context);
			localStorage.setItem(`nntune.library.favorites.v1.${context}`, JSON.stringify(['recording-a']));
		});
		await page.route('**/api/library', (route) => route.fulfill({ json: rows }));
		await page.goto(path);
		if (path === '/library') {
			await expect(page.getByTestId('library-item')).toHaveCount(3);
			await expect(page.getByTestId('favorite-item')).toHaveCount(1);
		} else {
			const items = page.locator('main ul').first().locator(':scope > li');
			await expect(items).toHaveCount(3);
			await expect(items).toContainText(['Echo One', 'Echo Two', 'Echo Three']);
		}
		expect(errors).toEqual([]);
	});
}
