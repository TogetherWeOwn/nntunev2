import { expect, test, type Locator, type Page } from '@playwright/test';
import { isListenContextRequest } from './fixtures/listen-context.js';

/**
 * Builder-owned review-round pins for per-entry demo history management.
 * The paired bank remains acceptance-owned with authorized retirement amendments.
 * This file pins surviving action names, retired action absence, and
 * narrow-screen touch targets.
 */
type Entry = { trackId: string; playedAt: number };
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
		if (isListenContextRequest(request, origin.origin)) return route.fulfill({ status: 401 });
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


test('BRM-02 recent actions retain names without retired controls', async ({ page, baseURL }) => {
	await open(page, baseURL!, {
		history: [{ trackId: 'second-signal-missing', playedAt: 400 }, ...HISTORY_ENTRIES]
	});
	const section = page.getByTestId('recently-played');
	await expect(row(page, 'second-signal')).toBeVisible();
	await expect(row(page, 'second-signal-missing')).toContainText('Unavailable track');
	await expect(row(page, 'second-signal-missing').getByRole('button', { name: /replay/i })).toHaveCount(0);
	await expect(row(page, 'second-signal').getByRole('button', { name: 'Replay Second Signal', exact: true })).toBeVisible();
	await expect(section.getByRole('button', { name: 'Clear recently played', exact: true })).toBeVisible();
	await expect(section.getByRole('button', { name: /add.*queue/i })).toHaveCount(0);
	await expect(section.getByRole('button', { name: /forget/i })).toHaveCount(0);
});

test('BRM-04 recent action touch targets are at least 44px on a narrow screen', async ({ page, baseURL }) => {
	await page.setViewportSize({ width: 375, height: 812 });
	await open(page, baseURL!);
	for (const action of [
		row(page, 'second-signal').getByRole('button', { name: 'Replay Second Signal', exact: true }),
		page.getByTestId('recently-played-clear')
	]) {
		await expect(action).toBeVisible();
		await expect(action).toBeEnabled();
		await action.scrollIntoViewIfNeeded();
		const box = await action.boundingBox();
		expect(box).not.toBeNull();
		if (!box) throw new Error('Recent action has no bounding box');
		expect(box.width).toBeGreaterThanOrEqual(44);
		expect(box.height).toBeGreaterThanOrEqual(44);
		expect(box.x).toBeGreaterThanOrEqual(0);
		expect(box.x + box.width).toBeLessThanOrEqual(375);
	}
});
