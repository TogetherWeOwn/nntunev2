import { expect, test, type Page } from '@playwright/test';

// Builder-owned regressions supplement the protected undo bank. Breaks caught:
// remounting live media on metadata undo, resurrecting a catalogue-deleted track,
// leaving keyboard focus behind a hiding queue filter, and seeking preserved
// media from a same-route share link after undo.
const ITEMS = [
	{ id: 'coral', title: 'Coral Night', streamUrl: '/test-audio/coral' },
	{ id: 'pine', title: 'Pine Morning', streamUrl: '/test-audio/pine' },
	{ id: 'ember', title: 'Ember Line', streamUrl: '/test-audio/ember' }
];

async function seed(page: Page, payload = { current: ITEMS }): Promise<void> {
	await page.route('**/api/library', (route) => route.fulfill({ json: payload.current }));
	await page.route('**/test-audio/**', (route) => route.abort());
	await page.goto('/library');
	for (const item of ITEMS) {
		await page.locator(`[data-testid="library-item"][data-id="${item.id}"]`).click();
	}
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

const undo = (page: Page) => page.getByRole('button', { name: 'Undo removal of Coral Night', exact: true });
const remove = (page: Page) => page.getByRole('button', { name: 'Remove Coral Night from queue', exact: true });

test('metadata undo preserves the mounted media source, intent and position', async ({ page }) => {
	await page.addInitScript(() => {
		Object.defineProperty(navigator, 'clipboard', {
			configurable: true,
			value: { writeText: async (text: string) => { document.documentElement.dataset.copiedLink = text; } }
		});
	});
	await seed(page, { current: [...ITEMS, { id: 'spruce', title: 'Spruce Day', streamUrl: '/test-audio/spruce' }] });
	await remove(page).click();
	const audio = page.getByTestId('audio-player');
	await expect(audio).toHaveAttribute('src', '/test-audio/pine');
	await audio.evaluate((element: HTMLAudioElement) => {
		element.setAttribute('data-original-source', 'yes');
		// A sentinel position catches writes without depending on network media.
		Object.defineProperty(element, 'currentTime', { configurable: true, value: 12, writable: true });
		for (const name of ['play', 'pause', 'load'] as const) {
			Object.defineProperty(element, name, {
				configurable: true,
				value: () => { element.setAttribute('data-transport-call', name); }
			});
		}
	});
	await undo(page).click();
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	await expect(page).toHaveTitle('Selected: Coral Night — Library — NNTune');
	await expect(audio).toHaveAttribute('data-original-source', 'yes');
	await expect(audio).toHaveAttribute('src', '/test-audio/pine');
	await expect(audio).toHaveJSProperty('currentTime', 12);
	await expect(audio).toHaveJSProperty('paused', true);
	await expect(audio).not.toHaveAttribute('data-transport-call');
	await expect(page.getByTestId('library-now-playing')).toContainText('Pine Morning');
	await page.getByTestId('library-list').getByRole('button', { name: 'Copy link Coral Night', exact: true }).click();
	await expect.poll(() => page.evaluate(() => {
		const url = new URL(document.documentElement.dataset.copiedLink!);
		return { track: url.searchParams.get('track'), t: url.searchParams.get('t') };
	})).toEqual({ track: 'coral', t: '0' });
	await page.locator('[data-testid="library-item"][data-id="spruce"]').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(4);
	await expect(audio).toHaveAttribute('data-original-source', 'yes');
	await expect(audio).toHaveAttribute('src', '/test-audio/pine');
});

test('removing the preserved source after undo switches away from the removed media', async ({ page }) => {
	await seed(page);
	await remove(page).click();
	const audio = page.getByTestId('audio-player');
	await expect(audio).toHaveAttribute('src', '/test-audio/pine');
	await audio.evaluate((element: HTMLAudioElement) => {
		element.setAttribute('data-original-source', 'yes');
	});
	await undo(page).click();
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	// An unrelated removal must keep the preserved source mounted.
	await page.getByRole('button', { name: 'Remove Ember Line from queue', exact: true }).click();
	await expect(audio).toHaveAttribute('data-original-source', 'yes');
	await expect(audio).toHaveAttribute('src', '/test-audio/pine');
	await page.getByRole('button', { name: 'Remove Pine Morning from queue', exact: true }).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(audio).toHaveAttribute('src', '/test-audio/coral');
	await expect(audio).not.toHaveAttribute('data-original-source');
	await expect(page.getByTestId('library-now-playing')).toContainText('Coral Night');
});

test('undoing the sole streamed track restores metadata without reloading its source', async ({ page }) => {
	await seed(page);
	for (const title of ['Pine Morning', 'Ember Line']) {
		await page.getByRole('button', { name: `Remove ${title} from queue`, exact: true }).click();
	}
	await remove(page).click();
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await undo(page).click();
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('refresh dropping only the removed track retires undo without touching survivors', async ({ page }) => {
	const payload = { current: ITEMS };
	await seed(page, payload);
	await remove(page).click();
	await expect(undo(page)).toBeVisible();
	payload.current = ITEMS.slice(1);
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expect(undo(page)).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expect(page.getByTestId('queue-announcement')).not.toContainText(/restored/i);
});

test('keyboard undo reveals a filtered restored row and focuses its Remove control', async ({ page }) => {
	await page.emulateMedia({ reducedMotion: 'reduce' });
	await seed(page);
	await remove(page).click();
	await page.getByLabel('Filter queue', { exact: true }).fill('pine');
	await undo(page).focus();
	await page.keyboard.press('Enter');
	await expect(page.getByLabel('Filter queue', { exact: true })).toHaveValue('');
	await expect(remove(page)).toBeFocused();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
});

test('same-route share link after undo seeks the link target, not the preserved source', async ({
	page
}) => {
	// Record every currentTime write per element: aborted test media never
	// reports a readable position, so the write log is the seek evidence.
	await page.addInitScript(() => {
		const time = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'currentTime')!;
		Object.defineProperty(HTMLMediaElement.prototype, 'currentTime', {
			configurable: true,
			get: time.get,
			set(value: number) {
				this.dataset.seekWrites = `${this.dataset.seekWrites ?? ''}${this.dataset.seekWrites ? ',' : ''}${value}`;
				time.set!.call(this, value);
			}
		});
	});
	await seed(page);
	await remove(page).click();
	const audio = page.getByTestId('audio-player');
	await expect(audio).toHaveAttribute('src', '/test-audio/pine');
	await audio.evaluate((element: HTMLAudioElement) => {
		element.setAttribute('data-original-source', 'yes');
	});
	await undo(page).click();
	await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
	// Client-side navigation to the restored current track, as browser
	// back/forward delivers it: the queue already shows Coral while the
	// mounted element still plays the preserved Pine source.
	await page.evaluate(() => {
		const link = document.createElement('a');
		link.id = 'undo-share-navigation';
		link.href = '/library?track=coral&t=30';
		link.textContent = 'Open track link';
		document.body.append(link);
	});
	await page.locator('#undo-share-navigation').click();
	await expect(page).toHaveURL(/\/library\?track=coral&t=30$/);
	// The explicit link remounts audio on its target; the pending seek lands
	// on Coral's element, never on the preserved Pine source.
	await expect(audio).toHaveAttribute('src', '/test-audio/coral');
	await expect(audio).not.toHaveAttribute('data-original-source');
	await expect(audio).toHaveAttribute('data-seek-writes', '30');
});

test('clearing upcoming releases a preserved source removed by the trim', async ({ page }) => {
	await seed(page);
	await remove(page).click();
	const audio = page.getByTestId('audio-player');
	await audio.evaluate((element: HTMLAudioElement) => element.setAttribute('data-original-source', 'yes'));
	await undo(page).click();
	await expect(audio).toHaveAttribute('src', '/test-audio/pine');
	await page.getByRole('button', { name: 'Clear upcoming', exact: true }).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(audio).toHaveAttribute('src', '/test-audio/coral');
	await expect(audio).not.toHaveAttribute('data-original-source');
	await expect(page.getByTestId('library-now-playing')).toContainText('Coral Night');
});

test('refresh pruning the preserved source switches to the selected track', async ({ page }) => {
	const payload = { current: ITEMS };
	await seed(page, payload);
	await remove(page).click();
	const audio = page.getByTestId('audio-player');
	await audio.evaluate((element: HTMLAudioElement) => element.setAttribute('data-original-source', 'yes'));
	await undo(page).click();
	await expect(audio).toHaveAttribute('src', '/test-audio/pine');
	payload.current = [ITEMS[0]!, ITEMS[2]!];
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expect(audio).toHaveAttribute('src', '/test-audio/coral');
	await expect(audio).not.toHaveAttribute('data-original-source');
	await expect(page.getByTestId('library-now-playing')).toContainText('Coral Night');
});

test('changing selection after undo releases preserved audio', async ({ page }) => {
	await seed(page);
	await remove(page).click();
	const audio = page.getByTestId('audio-player');
	await undo(page).click();
	await expect(audio).toHaveAttribute('src', '/test-audio/pine');
	await audio.evaluate((element: HTMLAudioElement) => element.setAttribute('data-original-source', 'yes'));
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Pine Morning');
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Ember Line');
	await expect(audio).toHaveAttribute('src', '/test-audio/ember');
	await expect(audio).not.toHaveAttribute('data-original-source');
});
