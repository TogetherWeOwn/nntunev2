import { expect, test, type Page } from '@playwright/test';

const track = { id: 'keys-coral', title: 'Keys Coral', streamUrl: '/api/demo/audio?track=coral' };
const audio = (page: Page) => page.getByTestId('audio-player');
const help = (page: Page) => page.getByTestId('shortcuts-button');
const status = (page: Page) => page.getByRole('region', { name: 'Player', exact: true }).getByRole('status');
const time = (page: Page) => audio(page).evaluate((node: HTMLAudioElement) => node.currentTime);

async function openLibrary(page: Page) {
	await page.route('**/api/library', (route) => route.fulfill({ json: [track] }));
	await page.goto(`/library?track=${track.id}&t=12`);
	await expect(status(page)).toHaveText('Paused');
	await expect(page.getByTestId('library-restart')).toBeEnabled();
	await expect.poll(() => time(page)).toBeCloseTo(12);
}

test('app shortcuts act once on the live native audio across routes and stop at teardown', async ({ page }) => {
	await openLibrary(page);
	const native = (await audio(page).elementHandle())!;
	await page.keyboard.press('j');
	await expect.poll(() => time(page)).toBeCloseTo(2);
	await page.keyboard.press('k');
	await expect(status(page)).toHaveText('Playing');
	await page.keyboard.press('k');
	await expect(status(page)).toHaveText('Paused');
	const pausedAt = await time(page);

	await page.getByTestId('shell-rail').getByRole('link', { name: 'Home', exact: true }).click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	await page.keyboard.press('l');
	await expect.poll(() => time(page)).toBeCloseTo(pausedAt + 10);
	await page.keyboard.press('0');
	await expect.poll(() => time(page)).toBeCloseTo(0);
	await expect(page.getByTestId('library-restart-status')).toContainText('Restarted Keys Coral');
	await expect(audio(page)).toHaveCount(1);
	expect(await native.evaluate((node: HTMLAudioElement) =>
		node === document.querySelector('audio') && node.controls && node.paused)).toBe(true);

	// A delegated link leaves the app shell without replacing the JS realm.
	await page.evaluate(() => {
		const exit = document.createElement('a');
		exit.href = '/pricing';
		exit.textContent = 'Leave shortcut fixture';
		document.body.append(exit);
	});
	await page.getByRole('link', { name: 'Leave shortcut fixture' }).click();
	await expect(page).toHaveURL(/\/pricing$/);
	await expect(page.getByRole('link', { name: 'Leave shortcut fixture' })).toBeAttached();
	await expect(audio(page)).toHaveCount(0);
	await expect(help(page)).toHaveCount(0);
	expect(await page.evaluate(() => ['k', 'l', '0', '?'].every((key) => document.body.dispatchEvent(
		new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true })
	)))).toBe(true);
});

test('app shortcut help lists only supported bindings, restores focus and respects key ownership', async ({ page }) => {
	await openLibrary(page);
	const play = page.getByRole('button', { name: 'Play', exact: true });
	await play.focus();
	await page.keyboard.press('?');
	const dialog = page.getByRole('dialog', { name: 'Keyboard shortcuts' });
	const close = dialog.getByRole('button', { name: 'Close' });
	await expect(dialog.locator('kbd')).toHaveText(['k', 'j', 'l', '0', '?', 'Escape']);
	await expect(close).toBeFocused();
	for (const key of ['Tab', 'Shift+Tab', 'j', 'k', '0']) await page.keyboard.press(key);
	await expect(close).toBeFocused();
	await expect(status(page)).toHaveText('Paused');
	expect(await time(page)).toBeCloseTo(12);
	await page.keyboard.press('Escape');
	await expect(dialog).toHaveCount(0);
	await expect(play).toBeFocused();
	await help(page).click();
	await close.click();
	await expect(help(page)).toBeFocused();

	expect(await page.evaluate(() => {
		const send = (target: EventTarget, key: string, flags?: KeyboardEventInit) => target.dispatchEvent(
			new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...flags })
		);
		const editable = document.createElement('div');
		editable.contentEditable = 'true';
		const child = editable.appendChild(document.createElement('span'));
		const select = document.createElement('select');
		const field = document.createElement('input');
		const modal = document.createElement('dialog');
		document.body.append(editable, select, field, modal);
		const native = document.querySelector('audio[controls]')!;
		const passed = [child, select, field, native].every((target) => send(target, '0')) &&
			[' ', 'ArrowLeft', 'ArrowRight', 'Enter'].every((key) => send(native, key)) &&
			['n', 'p', 'm', 's', 'r'].every((key) => send(document.body, key)) &&
			[{ ctrlKey: true }, { metaKey: true }, { altKey: true }, { isComposing: true }, { repeat: true }]
				.every((flags) => send(document.body, '0', flags));
		modal.showModal();
		const yielded = send(modal, '0') && send(modal, '?') && send(modal, 'Escape');
		modal.close();
		for (const node of [editable, select, field, modal]) node.remove();
		return passed && yielded;
	})).toBe(true);
	expect(await time(page)).toBeCloseTo(12);
	await play.press('Space');
	await expect(status(page)).toHaveText('Playing');
	await page.getByRole('button', { name: 'Pause', exact: true }).press('Enter');
	await expect(status(page)).toHaveText('Paused');
});
