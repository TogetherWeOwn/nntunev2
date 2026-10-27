import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Builder-owned complement to the protected `library-favorites-export.spec.ts`
 * acceptance bank (which stays untouched). It pins the lifecycle guards the
 * bank cannot reach because its fake clipboard settles in the same task and
 * no case crosses an auth boundary or leaves the page:
 * - feedback is fenced to the arranged list it was exported from;
 * - a superseded pending copy cannot overwrite newer feedback;
 * - an auth transition clears feedback and releases the retained Blob URL;
 * - leaving the page releases the retained Blob URL.
 * Real library page and storage; only HTTP is mocked.
 */

const ITEMS = [
	{ id: 'life-z', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'life-a', title: 'Alpha Song', artist: 'Cedar' },
	{ id: 'life-m', title: 'Mike Song', artist: 'Harbor Lights' }
];
const ALL = ['life-z', 'life-a', 'life-m'];
const CONTEXT = 'favorites-export-lifecycle';
const CONTEXT_KEY = 'nntune.library.auth-context.v1';
const KEY = `nntune.library.favorites.v1.${CONTEXT}`;
const TRANSITION = 'nntune.library.auth-transition.v1';

function section(page: Page): Locator {
	return page.getByTestId('favorites-section');
}
function copyButton(page: Page): Locator {
	return section(page).getByTestId('favorites-export-copy');
}
function downloadButton(page: Page): Locator {
	return section(page).getByTestId('favorites-export-download');
}
function statusRegion(page: Page): Locator {
	return section(page).getByTestId('favorites-export-status');
}
function unstar(page: Page, id: string): Locator {
	return page
		.locator(`[data-testid="library-item"][data-id="${id}"]`)
		.getByRole('button', { name: /^Unstar / });
}
async function blobLive(page: Page): Promise<number> {
	return page.evaluate(
		() => (window as unknown as { __lifeBlob: { live: string[] } }).__lifeBlob.live.length
	);
}
async function pendingCopies(page: Page): Promise<number> {
	return page.evaluate(
		() => (window as unknown as { __lifePending: unknown[] }).__lifePending.length
	);
}
async function settleCopy(page: Page, index: number): Promise<void> {
	await page.evaluate(
		(i) => (window as unknown as { __lifePending: Array<() => void> }).__lifePending[i]!(),
		index
	);
}

async function open(page: Page, clipboard: 'immediate' | 'deferred'): Promise<void> {
	await page.addInitScript(
		({ contextKey, context, key, ids, mode }) => {
			if (!sessionStorage.getItem('test.favorites-export-lifecycle.seeded')) {
				sessionStorage.setItem('test.favorites-export-lifecycle.seeded', '1');
				sessionStorage.setItem(contextKey, context);
				localStorage.setItem(key, JSON.stringify(ids));
			}
			const pending: Array<() => void> = [];
			(window as unknown as { __lifePending: typeof pending }).__lifePending = pending;
			Object.defineProperty(navigator, 'clipboard', {
				value: {
					writeText: () =>
						mode === 'deferred'
							? new Promise<void>((resolve) => pending.push(resolve))
							: Promise.resolve()
				},
				configurable: true
			});
			const tracker = { live: [] as string[] };
			(window as unknown as { __lifeBlob: typeof tracker }).__lifeBlob = tracker;
			const createObjectURL = URL.createObjectURL.bind(URL);
			const revokeObjectURL = URL.revokeObjectURL.bind(URL);
			URL.createObjectURL = (object: Blob | MediaSource) => {
				const url = createObjectURL(object);
				tracker.live.push(url);
				return url;
			};
			URL.revokeObjectURL = (url: string) => {
				tracker.live = tracker.live.filter((candidate) => candidate !== url);
				revokeObjectURL(url);
			};
			HTMLMediaElement.prototype.play = () => new Promise<void>(() => {});
			HTMLMediaElement.prototype.pause = () => {};
		},
		{ contextKey: CONTEXT_KEY, context: CONTEXT, key: KEY, ids: ALL, mode: clipboard }
	);
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(ITEMS)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('favorite-item')).toHaveCount(ALL.length);
}

test.beforeEach(async ({ page }) => {
	await page.context().route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (/^\/(?:api|v1)(?:\/|$)/.test(url.pathname)) return route.abort('blockedbyclient');
		return route.continue();
	});
});

async function crossAuthBoundaryInAnotherTab(page: Page): Promise<void> {
	const other = await page.context().newPage();
	await other.goto('/login');
	await other.evaluate((key) => {
		localStorage.setItem(key, 'favorites-export-lifecycle-transition');
		localStorage.removeItem(key);
	}, TRANSITION);
	await other.close();
}

test('[FAVEXPORT-L01] copy feedback disappears when the arranged favorites change', async ({
	page
}) => {
	await open(page, 'immediate');
	await copyButton(page).click();
	await expect(statusRegion(page)).toContainText(/copied/i);

	await unstar(page, 'life-m').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);
	await expect(
		statusRegion(page),
		'feedback for the old list must not describe the edited list'
	).not.toContainText(/copied/i);
});

test('[FAVEXPORT-L02] a copy that settles after a newer download cannot overwrite the download feedback', async ({
	page
}) => {
	await open(page, 'deferred');
	await copyButton(page).click();
	await expect.poll(() => pendingCopies(page)).toBe(1);

	await downloadButton(page).click();
	await expect(statusRegion(page)).toContainText(/download/i);

	await settleCopy(page, 0);
	// Give the late settlement a turn to (wrongly) write its status.
	await page.evaluate(() => new Promise((resolve) => setTimeout(resolve, 100)));
	await expect(statusRegion(page), 'late copy must not replace the download status').toContainText(
		/download/i
	);
	await expect(statusRegion(page)).not.toContainText(/copied/i);
});

test('[FAVEXPORT-L03] a copy that settles after the favorites changed shows no stale success', async ({
	page
}) => {
	await open(page, 'deferred');
	await copyButton(page).click();
	await expect.poll(() => pendingCopies(page)).toBe(1);

	await unstar(page, 'life-m').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);

	await settleCopy(page, 0);
	await page.evaluate(() => new Promise((resolve) => setTimeout(resolve, 100)));
	await expect(
		statusRegion(page),
		'a late copy of the old list must not announce success for the edited list'
	).not.toContainText(/copied/i);
});

test('[FAVEXPORT-L04] an auth transition clears export feedback and releases the retained Blob URL', async ({
	page
}) => {
	await open(page, 'immediate');
	await downloadButton(page).click();
	await expect(statusRegion(page)).toContainText(/download/i);
	await expect.poll(() => blobLive(page)).toBe(1);

	await crossAuthBoundaryInAnotherTab(page);

	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect
		.poll(() => blobLive(page), 'the previous account export URL must be revoked')
		.toBe(0);
	// The emptied Favorites section takes its export feedback with it.
	await expect(statusRegion(page)).toHaveCount(0);
	await expect(page.getByTestId('favorites-export-fallback')).toHaveCount(0);
});

test('[FAVEXPORT-L05] leaving the page releases the retained Blob URL', async ({ page }) => {
	await open(page, 'immediate');
	await downloadButton(page).click();
	await expect.poll(() => blobLive(page)).toBe(1);

	// Client-side navigation keeps the document, so the page teardown is
	// the only thing that can revoke the retained URL.
	await page.getByRole('link', { name: 'Select NZB locally', exact: true }).click();
	await expect(page).toHaveURL(/\/library\/import/);
	await expect.poll(() => blobLive(page)).toBe(0);
});
