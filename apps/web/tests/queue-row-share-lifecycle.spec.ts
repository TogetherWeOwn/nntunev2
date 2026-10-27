import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned supplement to the protected `queue-row-share.spec.ts`.
 * It pins three queue-share mechanisms the protected bank cannot see:
 * feedback hidden on any queue change, the auth-teardown generation fence
 * (no previous-session title resurrected), and code-point-safe title
 * truncation in the announcement.
 */

const ITEMS = [
	{ id: 'qsl-coral', title: 'Coral Night', artist: 'Mira Vale', streamUrl: '/test-audio/qsl-coral' },
	{ id: 'qsl-pine', title: 'Pine Morning', artist: 'North Field', streamUrl: '/test-audio/qsl-pine' }
];

// 68 ASCII units, one astral code point at UTF-16 units 68-69, then a long tail.
const ASTRAL_TITLE = `${'a'.repeat(68)}\u{1F600}${'b'.repeat(30)}`;

type ClipboardMode = 'fulfilled' | 'missing' | 'delayed';

async function openLibrary(
	page: Page,
	options: { items?: unknown; clipboard?: ClipboardMode } = {}
) {
	const payload: { current: unknown } = { current: options.items ?? ITEMS };
	const clipboard = options.clipboard ?? 'fulfilled';
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(payload.current)
		})
	);
	await page.route('**/test-audio/**', () => {});
	await page.addInitScript((mode: ClipboardMode) => {
		const control: { deferred: { resolve: () => void } | null } = { deferred: null };
		(window as unknown as { __qslControl: unknown }).__qslControl = control;
		if (mode === 'missing') {
			Object.defineProperty(navigator, 'clipboard', { value: undefined, configurable: true });
		} else {
			const writeText = () =>
				mode === 'delayed'
					? new Promise<void>((resolve) => {
							control.deferred = { resolve };
						})
					: Promise.resolve();
			Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
		}
		HTMLMediaElement.prototype.play = () => new Promise<void>(() => {});
		HTMLMediaElement.prototype.pause = () => {};
	}, clipboard);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(
		(options.items ?? ITEMS) instanceof Array ? ((options.items ?? ITEMS) as unknown[]).length : 0
	);
	return payload;
}

function queueRow(page: Page, id: string) {
	return page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);
}

async function queueTrack(page: Page, id: string, title: string) {
	const escaped = title.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	await page
		.locator(`[data-testid="library-item"][data-id="${id}"]`)
		.getByRole('button', { name: new RegExp(`^${escaped}( — |$)`) })
		.click();
}

async function resolveDelayedWrite(page: Page) {
	await page.evaluate(() => {
		const control = (
			window as unknown as { __qslControl: { deferred: { resolve: () => void } | null } }
		).__qslControl;
		if (!control.deferred) throw new Error('no deferred clipboard write is pending');
		control.deferred.resolve();
	});
}

test('[QSL-01] a completed copy announcement is hidden once its row leaves the queue', async ({
	page
}) => {
	await openLibrary(page);
	await queueTrack(page, 'qsl-coral', 'Coral Night');
	await queueTrack(page, 'qsl-pine', 'Pine Morning');
	await expect(page.getByTestId('queue-item')).toHaveCount(2);

	await queueRow(page, 'qsl-pine').getByTestId('queue-copy-link').click();
	const status = page.getByTestId('queue-share-status');
	await expect(status).toContainText(/copied link for pine morning/i);

	await queueRow(page, 'qsl-pine')
		.getByRole('button', { name: 'Remove Pine Morning from queue', exact: true })
		.click();
	await expect(queueRow(page, 'qsl-pine')).toHaveCount(0);
	await expect(status, 'a removed row must not keep its copy confirmation').toHaveText('');

	await queueRow(page, 'qsl-coral').getByTestId('queue-copy-link').click();
	await expect(status).toContainText(/copied link for coral night/i);
	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(status, 'Clear queue must not keep a stale confirmation').toHaveText('');
});

test('[QSL-02] the manual-copy fallback is hidden once the queue changes', async ({ page }) => {
	await openLibrary(page, { clipboard: 'missing' });
	await queueTrack(page, 'qsl-coral', 'Coral Night');
	await queueTrack(page, 'qsl-pine', 'Pine Morning');
	await expect(page.getByTestId('queue-item')).toHaveCount(2);

	await queueRow(page, 'qsl-pine').getByTestId('queue-copy-link').click();
	await expect(page.getByTestId('queue-share-fallback')).toBeVisible();
	await expect(page.getByTestId('queue-share-instructions')).toBeVisible();

	await queueRow(page, 'qsl-pine')
		.getByRole('button', { name: 'Remove Pine Morning from queue', exact: true })
		.click();
	await expect(queueRow(page, 'qsl-pine')).toHaveCount(0);
	await expect(
		page.getByTestId('queue-share-fallback'),
		'a removed row must not keep its fallback link'
	).toHaveCount(0);
	await expect(page.getByTestId('queue-share-instructions')).toHaveCount(0);
});

test('[QSL-03] auth teardown leaves no previous-session title in the queue status, even after a late write and Retry', async ({
	page
}) => {
	const payload = await openLibrary(page, { clipboard: 'delayed' });
	await queueTrack(page, 'qsl-coral', 'Coral Night');
	await queueTrack(page, 'qsl-pine', 'Pine Morning');
	await expect(page.getByTestId('queue-item')).toHaveCount(2);

	await queueRow(page, 'qsl-pine').getByTestId('queue-copy-link').click();

	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	await resolveDelayedWrite(page);
	await page.waitForTimeout(200);
	const status = page.getByTestId('queue-share-status');
	await expect(
		status,
		'a write finishing after teardown must stay silent, not name the old track'
	).toHaveText('');
	await expect(page.getByTestId('queue-share-fallback')).toHaveCount(0);

	payload.current = [{ id: 'qsl-bravo', title: 'Bravo Song' }];
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-list')).toContainText('Bravo Song');
	await expect(status).toHaveText('');
	await expect(page.getByTestId('queue-share-fallback')).toHaveCount(0);
});

test('[QSL-04] a long title with an astral character is truncated by code point, never a lone surrogate', async ({
	page
}) => {
	await openLibrary(page, {
		items: [{ id: 'qsl-long', title: ASTRAL_TITLE, artist: 'Long Artist', streamUrl: '/test-audio/qsl-long' }]
	});
	await queueTrack(page, 'qsl-long', ASTRAL_TITLE);
	await expect(page.getByTestId('queue-item')).toHaveCount(1);

	await queueRow(page, 'qsl-long').getByTestId('queue-copy-link').click();
	const status = page.getByTestId('queue-share-status');
	await expect(status).toContainText(/^\s*Copied link for /);
	const text = ((await status.textContent()) ?? '').trim();

	expect(text.length, `announcement must stay concise (actual ${text.length})`).toBeLessThan(100);
	expect(text, 'a long title must be truncated with an ellipsis').toContain('…');
	expect(text, 'the astral character before the cut must survive whole').toContain('\u{1F600}');
	expect(
		/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/.test(text),
		'announcement must not contain a lone surrogate'
	).toBe(false);
});
