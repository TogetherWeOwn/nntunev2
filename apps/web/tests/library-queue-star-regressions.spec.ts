import { expect, test, type Locator, type Page } from '@playwright/test';
import { openQueue } from './helpers/queue-host';

/**
 * Builder-owned regression coverage, safe for the Builder to extend.
 * Supplements library-queue-star.spec.ts without editing that protected bank.
 * Pins exact action labels, replacement announcements and rendered touch size.
 *
 * Narrow exception: the Engineering Lead authorized a one-time migration, in
 * the same pull request, of this file's physical queue host, opening, locator
 * and lifecycle setup to the persistent shell's Queue panel. Every assertion
 * and behavior vector remains protected, and an independent equivalence
 * review is required.
 */
const TRACKS = [
	{ id: 'queue-star-regression-zulu', title: 'Zulu Song' },
	{ id: 'queue-star-regression-alpha', title: 'Alpha Song' }
];

async function seedQueue(page: Page): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(TRACKS)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(TRACKS.length);
	for (const track of TRACKS) {
		await page.locator(`[data-testid="library-item"][data-id="${track.id}"]`).click();
	}
	await openQueue(page);
	await expect(page.getByTestId('queue-item')).toHaveCount(TRACKS.length);
}

function queueStar(page: Page, id: string): Locator {
	return page
		.locator(`[data-testid="queue-item"][data-track-id="${id}"]`)
		.getByRole('button', { name: /star/i });
}

async function expectTouchTarget(toggle: Locator): Promise<void> {
	await expect(toggle).toBeVisible();
	const box = await toggle.boundingBox();
	expect(box, 'star toggle must have a rendered hit target').not.toBeNull();
	expect(box!.width, 'star toggle width must be at least 44px').toBeGreaterThanOrEqual(44);
	expect(box!.height, 'star toggle height must be at least 44px').toBeGreaterThanOrEqual(44);
}

test('queue star action labels name the exact next action in both states', async ({ page }) => {
	await seedQueue(page);

	for (const track of TRACKS) {
		const toggle = queueStar(page, track.id);
		await expect(toggle).toHaveAccessibleName(`Star ${track.title}`);
		await expect(toggle).toHaveAttribute('aria-pressed', 'false');

		await toggle.click();
		await expect(toggle).toHaveAccessibleName(`Unstar ${track.title}`);
		await expect(toggle).toHaveAttribute('aria-pressed', 'true');

		await toggle.click();
		await expect(toggle).toHaveAccessibleName(`Star ${track.title}`);
		await expect(toggle).toHaveAttribute('aria-pressed', 'false');
	}
});

test('unstarring replaces the entire previous queue announcement', async ({ page }) => {
	await seedQueue(page);

	const announcement = page.getByTestId('queue-announcement');
	await expect(announcement).toHaveCount(1);
	await queueStar(page, 'queue-star-regression-zulu').click();
	await expect(announcement).toHaveText('Starred Zulu Song');

	await queueStar(page, 'queue-star-regression-alpha').click();
	await expect(announcement).toHaveText('Starred Alpha Song');

	await queueStar(page, 'queue-star-regression-zulu').click();
	await expect(announcement).toHaveCount(1);
	await expect(announcement).toHaveText('Unstarred Zulu Song');
	await expect(announcement).not.toContainText('Alpha Song');
});

test.describe('queue star minimum touch targets', () => {
	test.use({ viewport: { width: 360, height: 800 }, hasTouch: true });

	test('every queue star renders at least 44px square before and after tapping', async ({ page }) => {
		await seedQueue(page);

		for (const track of TRACKS) {
			const toggle = queueStar(page, track.id);
			await expectTouchTarget(toggle);

			await toggle.tap();
			await expect(toggle).toHaveAttribute('aria-pressed', 'true');
			await expectTouchTarget(toggle);

			await toggle.tap();
			await expect(toggle).toHaveAttribute('aria-pressed', 'false');
			await expectTouchTarget(toggle);
		}
	});
});
