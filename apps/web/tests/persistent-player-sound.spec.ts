import { expect, test, type Page } from '@playwright/test';

const key = 'nntune:sound-preference:v1';
const tracks = [
	{ id: 'sound-a', title: 'Sound A', streamUrl: '/api/demo/audio' },
	{ id: 'sound-b', title: 'Sound B', streamUrl: '/api/demo/audio?track=second-signal' }
];

async function openLibrary(page: Page) {
	await page.route('**/api/library', (route) => route.fulfill({ json: tracks }));
	await page.goto('/library');
	await page.getByTestId('library-item').filter({ hasText: 'Sound A' }).click();
	const player = page.getByRole('region', { name: 'Player', exact: true });
	const audio = player.getByTestId('audio-player');
	await expect(audio).toHaveAttribute('controls', '');
	return { player, audio, volume: player.getByRole('slider', { name: /^Volume/ }),
		mute: player.getByRole('button', { name: 'Mute', exact: true }) };
}

function stored(page: Page) {
	return page.evaluate((name) => JSON.parse(localStorage.getItem(name) ?? 'null'), key);
}

test('keyboard and native sound stay in sync across routes, replacement and reload', async ({ page }) => {
	const { audio, volume, mute } = await openLibrary(page);
	await expect(volume).toHaveValue('100');
	await volume.press('Home');
	await volume.press('ArrowRight');
	await mute.press('Space');
	await expect(audio).toHaveJSProperty('volume', 0.01);
	await expect(audio).toHaveJSProperty('muted', true);
	await expect(audio).toHaveJSProperty('paused', true);
	await expect.poll(() => stored(page)).toEqual({ version: 1, volume: 0.01, muted: true });

	await audio.evaluate((node: HTMLAudioElement) => { node.volume = 0.42; });
	await expect(volume).toHaveValue('42');
	await expect(mute).toHaveAttribute('aria-pressed', 'true');
	const old = (await audio.elementHandle())!;
	const nav = page.getByRole('navigation', { name: 'Primary', exact: true });
	await nav.getByRole('link', { name: 'Home', exact: true }).click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(volume).toHaveValue('42');
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
	await nav.getByRole('link', { name: 'Your Library', exact: true }).click();
	await expect(page).toHaveURL(/\/library$/);
	expect(await old.evaluate((node) => node === document.querySelector('[data-testid="audio-player"]'))).toBe(true);

	await page.getByTestId('library-item').filter({ hasText: 'Sound B' }).click();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Sound B');
	await expect(audio).toHaveJSProperty('volume', 0.42);
	await expect(audio).toHaveJSProperty('muted', true);
	expect(await old.evaluate((node) => node.isConnected)).toBe(false);
	await page.reload();
	await page.getByTestId('library-item').filter({ hasText: 'Sound A' }).click();
	await expect(volume).toHaveValue('42');
	await expect(mute).toHaveAttribute('aria-pressed', 'true');
});

test('restricted volume falls back to system controls and auth purge fences the outgoing audio', async ({ page }) => {
	await page.addInitScript(() => {
		const descriptor = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'volume')!;
		Object.defineProperty(HTMLMediaElement.prototype, 'volume', {
			...descriptor,
			set() { throw new DOMException('System volume only', 'NotSupportedError'); }
		});
	});
	const { audio, player, volume, mute } = await openLibrary(page);
	await volume.fill('35');
	await expect(volume).toHaveValue('100');
	await expect(volume).toBeDisabled();
	await expect(player.getByText('Use system volume controls.', { exact: true })).toBeVisible();
	expect(await stored(page)).toBeNull();
	await mute.press('Space');
	await expect(audio).toHaveJSProperty('muted', true);
	await page.getByTestId('library-item').filter({ hasText: 'Sound B' }).click();
	await player.getByRole('button', { name: 'Play', exact: true }).click();
	await expect(player.getByRole('status').filter({ hasText: /^Playing$/ })).toBeVisible();
	await audio.evaluate((node: HTMLAudioElement) => { node.currentTime = node.duration - 0.2; });
	await expect(audio).toHaveAttribute('src', tracks[1]!.streamUrl);
	await expect(audio).toHaveJSProperty('muted', true);
	await expect(player.getByRole('status').filter({ hasText: /^Playing$/ })).toBeVisible();
	const outgoing = (await audio.elementHandle())!;
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate(() => { localStorage.setItem('nntune.library.auth-transition.v1', 'sound-test'); });
	await expect(audio).toHaveCount(0);
	await outgoing.evaluate((node) => {
		(node as HTMLAudioElement).muted = false;
		node.dispatchEvent(new Event('volumechange'));
	});
	expect(await stored(page)).toEqual({ version: 1, volume: 1, muted: true });
	await otherTab.close();
});
