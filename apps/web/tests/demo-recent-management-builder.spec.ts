import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Builder-owned review-round pins for per-entry demo history management.
 * The paired acceptance suite remains byte-identical; this file covers
 * review-found behavior around empty queues, row-specific names, unavailable
 * entry removal, and the narrow-screen touch target.
 */
const HISTORY = 'nntune:demo-recently-played:v1';
const QUEUE = 'nntune:demo-queue:v1';

type Entry = { trackId: string; playedAt: number };
type MediaProbe = { plays: number; pauses: number; loads: number };
type OpenOptions = {
	history?: Entry[];
	order?: string[];
	currentId?: string | null;
};

const HISTORY_ENTRIES: Entry[] = [
	{ trackId: 'third-light', playedAt: 300 },
	{ trackId: 'second-signal', playedAt: 200 },
	{ trackId: 'demo-track', playedAt: 100 }
];
const DEFAULT_ORDER = ['third-light', 'demo-track'];
const networkCalls = new WeakMap<Page, string[]>();

function row(page: Page, id: string): Locator {
	return page
		.getByTestId('recent-item')
		.filter({ has: page.locator(`[data-testid="recent-played-at-${id}"]`) });
}

async function history(page: Page): Promise<Entry[]> {
	return page.evaluate((key) => JSON.parse(sessionStorage.getItem(key) ?? '[]') as Entry[], HISTORY);
}

async function probe(page: Page): Promise<MediaProbe> {
	return page.evaluate(() => {
		const control = (window as unknown as { __recentManagementBuilder: MediaProbe })
			.__recentManagementBuilder;
		return { ...control };
	});
}

async function transport(page: Page) {
	const audio = page.getByTestId('demo-audio');
	return {
		queue: await page
			.getByTestId('queue-item')
			.evaluateAll((rows) => rows.map((item) => item.getAttribute('data-track-id'))),
		current: await page.getByTestId('now-playing').textContent(),
		status: await page.getByTestId('player-status').textContent(),
		audio: {
			src: await audio.getAttribute('src'),
			time: await audio.evaluate((element: HTMLAudioElement) => element.currentTime),
			paused: await audio.evaluate((element: HTMLAudioElement) => element.paused)
		},
		probe: await probe(page)
	};
}

async function open(page: Page, baseURL: string, options: OpenOptions = {}): Promise<void> {
	const origin = new URL(baseURL);
	const calls: string[] = [];
	networkCalls.set(page, calls);
	await page.route('**/*', (route) => {
		const request = route.request();
		const target = new URL(request.url());
		if (target.origin !== origin.origin) {
			calls.push(`external ${request.method()} ${target.pathname}`);
			return route.abort();
		}
		if (target.pathname === '/api/demo/audio') return;
		// Page-embedded image GETs (demo artwork placeholders) are static assets, not
		// backend calls; only mutating or xhr/fetch requests count, as in the sibling spec.
		if (request.method() !== 'GET' || ['xhr', 'fetch'].includes(request.resourceType())) {
			calls.push(`${request.method()} ${target.pathname}`);
			return route.abort();
		}
		return route.continue();
	});
	await page.addInitScript(
		({ historyEntries, order, currentId }) => {
			sessionStorage.setItem('nntune:demo-recently-played:v1', JSON.stringify(historyEntries));
			sessionStorage.setItem('nntune:demo-queue:v1', JSON.stringify({ orderIds: order, currentId }));
			const control = { plays: 0, pauses: 0, loads: 0 };
			Object.assign(window, { __recentManagementBuilder: control });
			HTMLMediaElement.prototype.play = function () {
				control.plays += 1;
				return Promise.resolve();
			};
			HTMLMediaElement.prototype.pause = function () {
				control.pauses += 1;
			};
			const nativeLoad = HTMLMediaElement.prototype.load;
			HTMLMediaElement.prototype.load = function () {
				control.loads += 1;
				return nativeLoad.call(this);
			};
		},
		{
			historyEntries: options.history ?? HISTORY_ENTRIES,
			order: options.order ?? DEFAULT_ORDER,
			currentId: options.currentId === undefined ? 'demo-track' : options.currentId
		}
	);
	await page.goto('/demo/player');
	await expect(page.getByTestId('recently-played')).toBeVisible();
}

test.afterEach(async ({ page }) => {
	expect(networkCalls.get(page) ?? [], 'no backend, telemetry, provider, or external requests').toEqual([]);
});

test('BRM-01 adding a recent entry to an empty queue selects without playing', async ({ page, baseURL }) => {
	await open(page, baseURL!, {
		history: [{ trackId: 'second-signal', playedAt: 200 }],
		order: [],
		currentId: null
	});
	await expect(page.getByTestId('player-status')).toHaveText('Status: empty');
	const playsBefore = (await probe(page)).plays;
	await row(page, 'second-signal').getByRole('button', { name: /add.*queue/i }).click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	expect((await probe(page)).plays).toBe(playsBefore);
});

test('BRM-02 recent actions expose the affected entry in their accessible names', async ({ page, baseURL }) => {
	await open(page, baseURL!, {
		history: [{ trackId: 'second-signal-missing', playedAt: 400 }, ...HISTORY_ENTRIES]
	});
	await expect(row(page, 'second-signal').getByRole('button', { name: 'Add Second Signal to queue', exact: true })).toBeVisible();
	await expect(row(page, 'second-signal').getByRole('button', { name: 'Forget Second Signal from recent history', exact: true })).toBeVisible();
	await expect(row(page, 'second-signal-missing').getByRole('button', {
		name: 'Forget unavailable entry second-signal-missing from recent history',
		exact: true
	})).toBeVisible();
});

test('BRM-03 forgetting an unavailable row removes only that history entry without transport changes', async ({ page, baseURL }) => {
	const seeded = [{ trackId: 'second-signal-missing', playedAt: 400 }, ...HISTORY_ENTRIES];
	await open(page, baseURL!, { history: seeded });
	const before = await transport(page);
	await row(page, 'second-signal-missing').getByRole('button', { name: /forget/i }).click();
	await expect(row(page, 'second-signal-missing')).toHaveCount(0);
	expect(await history(page)).toEqual(seeded.filter((entry) => entry.trackId !== 'second-signal-missing'));
	expect(await transport(page)).toEqual(before);
});

test('BRM-04 recent action touch targets are at least 44px on a narrow screen', async ({ page, baseURL }) => {
	await page.setViewportSize({ width: 375, height: 812 });
	await open(page, baseURL!);
	const action = row(page, 'second-signal').getByRole('button', { name: /add.*queue/i });
	await action.scrollIntoViewIfNeeded();
	const box = await action.boundingBox();
	expect(box).not.toBeNull();
	if (!box) return;
	expect(box.width).toBeGreaterThanOrEqual(44);
	expect(box.height).toBeGreaterThanOrEqual(44);
	expect(box.x).toBeGreaterThanOrEqual(0);
	expect(box.x + box.width).toBeLessThanOrEqual(375);
});
