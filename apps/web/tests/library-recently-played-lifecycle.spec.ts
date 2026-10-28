import { expect, test, type Page } from '@playwright/test';

// Acceptance-owned regressions: replay must upgrade a bulk-added metadata row,
// and auth changes must purge this tab even when Library is not mounted.
const HISTORY_KEY = 'nntune:library-recently-played:v1';
const AUTH_TRANSITION_KEY = 'nntune.library.auth-transition.v1';
const tracks = [
  {
    id: 'recents-amber',
    title: 'Amber sample',
    artist: 'Synthetic artist',
    album: 'Synthetic album',
    streamUrl: '/synthetic/recents-amber.wav'
  },
  {
    id: 'recents-cobalt',
    title: 'Cobalt sample',
    artist: 'Synthetic artist',
    album: 'Synthetic album',
    streamUrl: '/synthetic/recents-cobalt.wav'
  },
  {
    id: 'recents-violet',
    title: 'Violet sample',
    artist: 'Synthetic artist',
    album: 'Synthetic album',
    streamUrl: '/synthetic/recents-violet.wav'
  }
];

interface MediaCalls {
  play: number;
  pause: number;
  load: number;
  seeks: number[];
}

declare global {
  interface Window {
    __recentsLifecycleMedia: MediaCalls;
    __recentsLifecycleAuthEvents: { newValue: string | null; trusted: boolean }[];
    __recentsLifecycleDocument?: string;
  }
}

test.beforeEach(async ({ context, baseURL }) => {
  if (!baseURL) throw new Error('The synthetic acceptance fixture requires a local baseURL');
  const base = new URL(baseURL);
  if (!['localhost', '127.0.0.1', '[::1]'].includes(base.hostname)) {
    throw new Error('The synthetic acceptance fixture must not target a deployed service');
  }
  const origin = base.origin;
  await context.route('**/*', async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    if (url.origin !== origin) {
      await route.abort();
    } else if (url.pathname === '/api/library' && request.method() === 'GET') {
      await route.fulfill({ json: tracks });
    } else if (url.pathname.startsWith('/synthetic/')) {
      // No media bytes, network or decoder events: only explicit synthetic
      // playing events below can record history. Context teardown drops this request.
    } else if (url.pathname.startsWith('/api/')) {
      await route.abort();
    } else {
      await route.continue();
    }
  });

  await context.addInitScript((authKey) => {
    const calls = { play: 0, pause: 0, load: 0, seeks: [] as number[] };
    window.__recentsLifecycleMedia = calls;
    HTMLMediaElement.prototype.play = function () {
      calls.play += 1;
      return Promise.resolve();
    };
    HTMLMediaElement.prototype.pause = function () {
      calls.pause += 1;
    };
    HTMLMediaElement.prototype.load = function () {
      calls.load += 1;
    };
    const currentTime = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'currentTime');
    if (!currentTime?.get || !currentTime.set) throw new Error('Missing native currentTime accessors');
    const setCurrentTime = currentTime.set;
    Object.defineProperty(HTMLMediaElement.prototype, 'currentTime', {
      ...currentTime,
      set(value: number) {
        calls.seeks.push(value);
        setCurrentTime.call(this, value);
      }
    });
    window.__recentsLifecycleAuthEvents = [];
    window.addEventListener('storage', (event) => {
      if (event.key === authKey) {
        window.__recentsLifecycleAuthEvents.push({
          newValue: event.newValue,
          trusted: event.isTrusted
        });
      }
    });
  }, AUTH_TRANSITION_KEY);
});

async function openLibrary(page: Page) {
  await page.goto('/library');
  await expect(page.getByTestId('library-item')).toHaveCount(3);
}

async function recordCobaltPlaying(page: Page) {
  await page.locator('[data-testid="library-item"][data-id="recents-cobalt"] button.nt-library-row').click();
  const audio = page.getByTestId('audio-player');
  await expect(audio).toHaveAttribute('src', '/synthetic/recents-cobalt.wav');
  await expect(page.getByTestId('library-recent-item')).toHaveCount(0);
  await audio.evaluate((element) => {
    Object.defineProperty(element, 'paused', { value: false, configurable: true });
    Object.defineProperty(element, 'ended', { value: false, configurable: true });
    element.dispatchEvent(new Event('playing'));
  });
  await expect(page.getByTestId('library-recent-item')).toHaveCount(1);
  await expect(page.getByTestId('library-recent-item')).toContainText('Cobalt sample');
  await expect.poll(() => page.evaluate((key) => sessionStorage.getItem(key), HISTORY_KEY)).not.toBeNull();
}

async function queueIds(page: Page) {
  return page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
}

async function mediaCalls(page: Page) {
  return page.evaluate(() => window.__recentsLifecycleMedia);
}

async function flushRendering(page: Page) {
  await page.evaluate(() => new Promise<void>((resolve) => {
    requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
  }));
}

test('bulk-added history replay restores the library source without moving or duplicating queue rows', async ({ page }) => {
  await openLibrary(page);
  await recordCobaltPlaying(page);
  await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
  await expect(page.getByTestId('queue-item')).toHaveCount(0);
  await expect(page.getByTestId('library-recent-item')).toHaveCount(1);

  await page.getByRole('button', { name: /^Add displayed tracks to queue/ }).click();
  await expect.poll(() => queueIds(page)).toEqual(['recents-amber', 'recents-cobalt', 'recents-violet']);
  await expect(page.getByTestId('audio-player')).toHaveCount(0);
  await flushRendering(page);
  const beforeReplay = await mediaCalls(page);

  await page.getByTestId('library-recent-replay-recents-cobalt').click();
  await expect(page.getByTestId('audio-player')).toHaveAttribute('src', '/synthetic/recents-cobalt.wav');
  await expect.poll(() => queueIds(page)).toEqual(['recents-amber', 'recents-cobalt', 'recents-violet']);
  await flushRendering(page);
  expect(await mediaCalls(page), 'Replay is selection-only: no play, pause, load or seek').toEqual(beforeReplay);
});

test('a real second-tab auth broadcast removes off-route history before Library is remounted', async ({ page, context, baseURL }) => {
  await openLibrary(page);
  await recordCobaltPlaying(page);
  const recordedHistory = await page.evaluate((key) => sessionStorage.getItem(key), HISTORY_KEY);
  expect(recordedHistory).not.toBeNull();
  await page.evaluate(() => { window.__recentsLifecycleDocument = 'same-tab-document'; });

  const shell = page.locator('nav:has(a[href="/library/import"])');
  await shell.getByRole('link', { name: 'Select NZB locally', exact: true }).click();
  await expect(page).toHaveURL(new URL('/library/import', baseURL!).href);
  await expect(page.getByTestId('library-item')).toHaveCount(0);
  await expect(page.getByTestId('library-recent-item')).toHaveCount(0);
  expect(await page.evaluate(() => window.__recentsLifecycleDocument)).toBe('same-tab-document');
  expect(await page.evaluate((key) => sessionStorage.getItem(key), HISTORY_KEY)).toBe(recordedHistory);

  const otherTab = await context.newPage();
  await otherTab.goto('/login');
  // sessionStorage is tab-local: the receiver must clear its own bytes rather
  // than relying on the broadcasting tab to clear a shared history store.
  expect(await otherTab.evaluate((key) => sessionStorage.getItem(key), HISTORY_KEY)).toBeNull();
  await otherTab.evaluate((key) => {
    localStorage.setItem(key, 'synthetic-auth-transition');
    localStorage.removeItem(key);
  }, AUTH_TRANSITION_KEY);
  await expect.poll(() => page.evaluate(() => window.__recentsLifecycleAuthEvents)).toContainEqual({
    newValue: 'synthetic-auth-transition',
    trusted: true
  });
  await expect.poll(() => page.evaluate((key) => sessionStorage.getItem(key), HISTORY_KEY)).toBeNull();
  await expect(page).toHaveURL(new URL('/library/import', baseURL!).href);
  await otherTab.close();

  await shell.getByRole('link', { name: 'Library', exact: true }).click();
  await expect(page.getByTestId('library-item')).toHaveCount(3);
  await expect(page.getByTestId('library-recent-item')).toHaveCount(0);
  expect(await page.evaluate(() => window.__recentsLifecycleDocument)).toBe('same-tab-document');
});
