import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned regressions complementing the protected demo-tab-title bank.
 * Catch a raw-status title predicate and missing real-page teardown cleanup.
 * Only native media is mocked; title writes and SvelteKit lifecycles stay real.
 */
const STATIC_TITLE = 'NNTune — player resilience demo';
const SECOND_TITLE = 'Second Signal — Fixture Band';
const PRICING_TITLE = 'Plans and pricing — NNTune v2';

type TitleProbe = Window & { __tabTitleWrites: string[] };

async function openPlayer(page: Page): Promise<void> {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		HTMLMediaElement.prototype.play = function () {
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {
			Object.defineProperty(this, 'paused', { value: true, configurable: true });
		};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
}

async function navigateWithinDocument(page: Page, path: string): Promise<void> {
	// An ordinary same-origin anchor exercises the real client router, unlike
	// page.goto (which destroys the document and hides component cleanup).
	await page.evaluate((href) => {
		const link = document.createElement('a');
		link.id = 'tab-title-navigation';
		link.href = href;
		link.textContent = 'Navigate';
		document.body.append(link);
	}, path);
	await page.locator('#tab-title-navigation').click();
	await expect(page).toHaveURL(new RegExp(`${path}$`));
	await page.locator('#tab-title-navigation').evaluate((link) => link.remove());
}

test('buffering appends the paused suffix and playing recovery clears it', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-second-signal').click();
	// Wait for play() to settle before the event establishes has-played history.
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await page.getByTestId('demo-audio').evaluate((audio) => {
		audio.dispatchEvent(new Event('playing'));
	});
	await expect(page).toHaveTitle(SECOND_TITLE);

	await page.getByTestId('demo-audio').evaluate((audio) => {
		audio.dispatchEvent(new Event('waiting'));
	});
	await expect(page.getByTestId('player-status')).toHaveText('Status: buffering');
	await expect(page).toHaveTitle(`${SECOND_TITLE} (paused)`);

	await page.getByTestId('demo-audio').evaluate((audio) => {
		audio.dispatchEvent(new Event('playing'));
	});
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page).toHaveTitle(SECOND_TITLE);
});

test('each real page unmount restores once before the destination title, including after remount', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-second-signal').click();
	await expect(page).toHaveTitle(SECOND_TITLE);

	await page.evaluate(() => {
		const descriptor = Object.getOwnPropertyDescriptor(Document.prototype, 'title');
		if (!descriptor?.get || !descriptor.set) throw new Error('Native title accessor missing');
		const writes: string[] = [];
		(window as TitleProbe).__tabTitleWrites = writes;
		Object.defineProperty(Document.prototype, 'title', {
			...descriptor,
			set(this: Document, value: string) {
				writes.push(value);
				descriptor.set!.call(this, value);
			}
		});
	});

	for (let mount = 0; mount < 2; mount++) {
		await page.evaluate(() => { (window as TitleProbe).__tabTitleWrites.length = 0; });
		await navigateWithinDocument(page, '/pricing');
		await expect(page.getByRole('heading', { name: 'Plans and pricing', exact: true })).toBeVisible();
		await expect(page).toHaveTitle(PRICING_TITLE);

		// The probe survives only same-document navigation. Count the actual
		// cleanup write, not the final title that the next page can overwrite.
		// The current Svelte head runtime also writes through this native setter.
		const writes = await page.evaluate(() => (window as TitleProbe).__tabTitleWrites);
		expect(writes.filter((title) => title === STATIC_TITLE)).toEqual([STATIC_TITLE]);
		expect(writes.indexOf(STATIC_TITLE)).toBeLessThan(writes.indexOf(PRICING_TITLE));

		if (mount === 0) {
			await navigateWithinDocument(page, '/demo/player');
			await expect(page.getByTestId('queue-item')).toHaveCount(3);
			await expect(page).toHaveTitle(`${SECOND_TITLE} (paused)`);
			await page.getByTestId('queue-select-second-signal').click();
			await expect(page).toHaveTitle(SECOND_TITLE);
		}
	}
	await expect(page).toHaveTitle(PRICING_TITLE);
});
