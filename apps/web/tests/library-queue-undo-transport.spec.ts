import { expect, test, type Page } from '@playwright/test';

const items = [
  { id: 'coral', title: 'Coral Night', streamUrl: '/test-audio/coral' },
  { id: 'pine', title: 'Pine Morning', streamUrl: '/test-audio/pine' },
  { id: 'ember', title: 'Ember Line', streamUrl: '/test-audio/ember' }
];

async function setup(page: Page, tracks = items) {
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: async (text: string) => { document.documentElement.dataset.copied = text; } } });
    HTMLMediaElement.prototype.play = function () {
      document.documentElement.dataset.played = this.getAttribute('src') ?? '';
      document.documentElement.dataset.plays = String(Number(document.documentElement.dataset.plays ?? 0) + 1);
      Object.defineProperty(this, 'paused', { configurable: true, value: false });
      Object.defineProperty(this, 'ended', { configurable: true, value: false });
      return Promise.resolve();
    };
  });
  await page.route('**/api/library', route => route.fulfill({ json: tracks }));
  await page.route('**/test-audio/**', () => {});
  await page.goto('/library');
  for (const track of tracks) await page.locator(`[data-testid="library-item"][data-id="${track.id}"]`).click();
}
async function removeUndo(page: Page, title = 'Coral Night') {
  await page.getByRole('button', { name: `Remove ${title} from queue`, exact: true }).click();
  await page.getByRole('button', { name: `Undo removal of ${title}`, exact: true }).click();
}

test('queue links use the mounted track position rather than restored selection', async ({ page }) => {
  await setup(page); await removeUndo(page);
  await page.getByTestId('audio-player').evaluate(el => Object.defineProperty(el, 'currentTime', { configurable: true, writable: true, value: 12 }));
  await page.getByTestId('queue-item').filter({ hasText: 'Coral Night' }).getByRole('button', { name: 'Copy link Coral Night' }).click();
  await expect.poll(() => page.evaluate(() => new URL(document.documentElement.dataset.copied!).searchParams.get('t'))).toBe('0');
  await page.getByTestId('queue-item').filter({ hasText: 'Pine Morning' }).getByRole('button', { name: 'Copy link Pine Morning' }).click();
  await expect.poll(() => page.evaluate(() => new URL(document.documentElement.dataset.copied!).searchParams.get('t'))).toBe('12');
});

test('explicit play from top releases preserved audio', async ({ page }) => {
  await setup(page); await removeUndo(page);
  await page.getByRole('button', { name: 'Play displayed from the top' }).click();
  await expect(page.getByTestId('audio-player')).toHaveAttribute('src', '/test-audio/coral');
  await expect.poll(() => page.evaluate(() => document.documentElement.dataset.played)).toBe('/test-audio/coral');
});

test('restart seeks and names the mounted track after undo', async ({ page }) => {
  await setup(page); await removeUndo(page);
  await page.getByTestId('audio-player').evaluate(el => {
    Object.defineProperty(el, 'duration', { configurable: true, value: 100 });
    Object.defineProperty(el, 'currentTime', { configurable: true, writable: true, value: 12 });
    Object.defineProperty(el, 'seekable', { configurable: true, value: { length: 1, start: () => 0, end: () => 100 } });
    el.dispatchEvent(new Event('timeupdate'));
  });
  await expect(page.getByTestId('library-restart')).toBeEnabled();
  await page.getByTestId('library-restart').click();
  await expect(page.getByTestId('audio-player')).toHaveJSProperty('currentTime', 0);
  await expect(page.getByTestId('library-restart-status')).toHaveText('Restarted Pine Morning.');
});

for (const [mode, expected] of [['one', 'pine'], ['off', 'ember']] as const) {
  test(`natural end after undo applies ${mode} from the audible queue position`, async ({ page }) => {
    await setup(page); await page.getByTestId('player-repeat').selectOption(mode); await removeUndo(page);
    await page.getByTestId('audio-player').evaluate(el => {
      void (el as HTMLAudioElement).play(); el.dispatchEvent(new Event('play'));
      Object.defineProperty(el, 'paused', { configurable: true, value: true });
      Object.defineProperty(el, 'ended', { configurable: true, value: true });
      el.dispatchEvent(new Event('pause')); el.dispatchEvent(new Event('ended'));
    });
    await expect.poll(() => page.evaluate(() => Number(document.documentElement.dataset.plays))).toBe(2);
    await expect(page.getByTestId('audio-player')).toHaveAttribute('src', `/test-audio/${expected}`);
    await expect(page.getByTestId('now-playing')).toContainText(expected === 'pine' ? 'Pine Morning' : 'Ember Line');
  });
}

test('explicitly selecting an unloaded restored sole track reloads it', async ({ page }) => {
  await setup(page, [items[0]!]); await removeUndo(page);
  await expect(page.getByTestId('audio-player')).toHaveCount(0);
  await page.getByRole('button', { name: 'Select Coral Night', exact: true }).click();
  await expect(page.getByTestId('audio-player')).toHaveAttribute('src', '/test-audio/coral');
});

test('restored announcement never splits a surrogate pair', async ({ page }) => {
  const title = 'a'.repeat(98) + '😀x';
  await setup(page, [{ id: 'coral', title, streamUrl: '/test-audio/coral' }]);
  await removeUndo(page, title);
  await expect(page.getByTestId('queue-announcement')).toContainText('restored to the queue.');
  expect(await page.getByTestId('queue-announcement').evaluate(el => (el.textContent ?? '').isWellFormed())).toBe(true);
});

test('undo preserves restart availability on unchanged mounted audio', async ({ page }) => {
  await setup(page);
  await page.getByRole('button', { name: 'Remove Coral Night from queue', exact: true }).click();
  await page.getByTestId('audio-player').evaluate(el => {
    Object.defineProperty(el, 'duration', { configurable: true, value: 100 });
    Object.defineProperty(el, 'currentTime', { configurable: true, writable: true, value: 12 });
    Object.defineProperty(el, 'seekable', { configurable: true, value: { length: 1, start: () => 0, end: () => 100 } });
    el.dispatchEvent(new Event('timeupdate'));
  });
  await expect(page.getByTestId('library-restart')).toBeEnabled();
  await page.getByRole('button', { name: 'Undo removal of Coral Night', exact: true }).click();
  await expect(page.getByTestId('library-restart')).toBeEnabled();
});

test('repeat queue wraps from a preserved tail track after undo', async ({ page }) => {
  await setup(page);
  await page.getByTestId('player-repeat').selectOption('queue');
  await page.getByTestId('player-next').click();
  await removeUndo(page, 'Pine Morning');
  await expect(page.getByTestId('audio-player')).toHaveAttribute('src', '/test-audio/ember');
  await page.getByTestId('audio-player').evaluate(el => {
    void (el as HTMLAudioElement).play(); el.dispatchEvent(new Event('play'));
    Object.defineProperty(el, 'paused', { configurable: true, value: true });
    Object.defineProperty(el, 'ended', { configurable: true, value: true });
    el.dispatchEvent(new Event('pause')); el.dispatchEvent(new Event('ended'));
  });
  await expect.poll(() => page.evaluate(() => Number(document.documentElement.dataset.plays))).toBe(2);
  await expect(page.getByTestId('audio-player')).toHaveAttribute('src', '/test-audio/coral');
  await expect(page.getByTestId('now-playing')).toContainText('Coral Night');
});

test('clicking the restored sole library row reloads it without playing', async ({ page }) => {
  await setup(page, [items[0]!]); await removeUndo(page);
  await expect(page.getByTestId('audio-player')).toHaveCount(0);
  await page.locator('[data-testid="library-item"][data-id="coral"]').click();
  await expect(page.getByTestId('audio-player')).toHaveAttribute('src', '/test-audio/coral');
  expect(await page.evaluate(() => document.documentElement.dataset.plays)).toBeUndefined();
});

test('play from top keeps restart availability when undo retained the same source', async ({ page }) => {
  await setup(page);
  await removeUndo(page, 'Pine Morning');
  await page.getByTestId('audio-player').evaluate(el => {
    Object.defineProperty(el, 'duration', { configurable: true, value: 100 });
    Object.defineProperty(el, 'currentTime', { configurable: true, writable: true, value: 12 });
    Object.defineProperty(el, 'seekable', { configurable: true, value: { length: 1, start: () => 0, end: () => 100 } });
    el.dispatchEvent(new Event('timeupdate'));
  });
  await expect(page.getByTestId('library-restart')).toBeEnabled();
  await page.getByRole('button', { name: 'Play displayed from the top' }).click();
  await expect(page.getByTestId('library-restart')).toBeEnabled();
});
