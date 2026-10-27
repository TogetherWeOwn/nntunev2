import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Complements `library-favorites-truth.spec.ts` (acceptance-owned, left
 * untouched). This file is implementation-owned and safe to extend: it pins
 * the summary guard and count, the retry-success status and focus, and the
 * access-refusal reset of the unsaved flag, each of which could be swapped or
 * deleted while the acceptance bank stayed green.
 *
 * Case IDs (stable): FAVGAP-G01 summary absent on a fully loaded shelf and
 * exact on a mixed one, G02 retry success status and heading focus, G03
 * access refusal clears the unsaved flag.
 */

const ITEMS = [
	{ id: 'gap-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'gap-alpha', title: 'Alpha Song' },
	{ id: 'gap-mike', title: 'Mike Song', artist: 'Harbor Lights' }
];
const GHOST_A = 'gap-ghost-alpha';
const GHOST_B = 'gap-ghost-beta';
const CONTEXT = 'gap-account-a';
const CONTEXT_KEY = 'nntune.library.auth-context.v1';
const KEY_BASE = 'nntune.library.favorites.v1';
const KEY = `${KEY_BASE}.${CONTEXT}`;

function section(page: Page): Locator {
	return page.getByTestId('favorites-section');
}
function notice(page: Page): Locator {
	return section(page).getByTestId('favorites-save-notice');
}
function summary(page: Page): Locator {
	return section(page).getByTestId('favorites-summary');
}
function starButton(page: Page, id: string): Locator {
	return page
		.locator(`[data-testid="library-item"][data-id="${id}"]`)
		.getByRole('button', { name: /^(?:Unstar|Star) / });
}

async function openLibrary(
	page: Page,
	options: { stored?: string[]; reply?: { status: number } } = {}
): Promise<void> {
	await page.addInitScript(
		({ contextKey, context, key, stored }) => {
			if (window.sessionStorage.getItem('test.gap.seeded')) return;
			window.sessionStorage.setItem('test.gap.seeded', '1');
			window.sessionStorage.setItem(contextKey, context);
			if (stored) window.localStorage.setItem(key, JSON.stringify(stored));
		},
		{ contextKey: CONTEXT_KEY, context: CONTEXT, key: KEY, stored: options.stored ?? null }
	);
	const reply = options.reply ?? { status: 200 };
	await page.route('**/api/library', (route) =>
		reply.status === 200
			? route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(ITEMS) })
			: route.fulfill({ status: reply.status, contentType: 'application/json', body: '{}' })
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(section(page)).toBeVisible();
}

async function failFavoritesWrites(page: Page): Promise<void> {
	await page.evaluate((base) => {
		const setItem = Storage.prototype.setItem;
		Storage.prototype.setItem = function (key, value) {
			if (this === window.localStorage && key.startsWith(`${base}.`)) {
				throw new DOMException('Favorites writes unavailable', 'QuotaExceededError');
			}
			return setItem.call(this, key, value);
		};
		Reflect.set(window, '__gapRestore', () => {
			Storage.prototype.setItem = setItem;
		});
	}, KEY_BASE);
}

async function restoreWrites(page: Page): Promise<void> {
	await page.evaluate(() => (Reflect.get(window, '__gapRestore') as () => void)());
}

test('[FAVGAP-G01] no summary on a fully loaded shelf; exact absent count on a mixed one', async ({
	page
}) => {
	await openLibrary(page, { stored: ['gap-zulu', 'gap-alpha'] });
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);
	await expect(summary(page), 'every saved favorite is loaded: nothing to report').toHaveCount(0);
});

test('[FAVGAP-G01b] mixed shelf counts only the absent ids,', async ({
	page
}) => {
	await openLibrary(page, { stored: [GHOST_A, GHOST_B, 'gap-zulu'] });
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect(summary(page)).toContainText('1 shown below.');
	await expect(summary(page)).toContainText('2 saved favorites are not in the loaded tracks.');
	await starButton(page, 'gap-alpha').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);
	await expect(summary(page)).toContainText('2 shown below.');
	await expect(summary(page)).toContainText('2 saved favorites are not in the loaded tracks.');
});

test('[FAVGAP-G02] a successful Save again announces success and focuses the shelf heading', async ({
	page
}) => {
	await openLibrary(page, { stored: ['gap-zulu'] });
	await failFavoritesWrites(page);
	await starButton(page, 'gap-alpha').click();
	await expect(notice(page)).toBeVisible();
	await restoreWrites(page);
	await notice(page).getByRole('button', { name: 'Save again' }).click();
	await expect(notice(page)).toHaveCount(0);
	await expect(section(page).getByRole('status').filter({ hasText: 'Favorites saved on this device.' })).toBeVisible();
	await expect(section(page).getByRole('heading', { name: 'Favorites' })).toBeFocused();
});

test('[FAVGAP-G03] an access-refusal refresh clears the unsaved flag before the next load', async ({
	page
}) => {
	const reply = { status: 200 };
	await openLibrary(page, { reply, stored: ['gap-zulu'] });
	await failFavoritesWrites(page);
	await starButton(page, 'gap-alpha').click();
	await expect(notice(page)).toBeVisible();
	await restoreWrites(page);

	reply.status = 401;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByRole('alert')).toBeVisible();
	await expect(notice(page), 'the refusal clears the pending notice').toHaveCount(0);

	reply.status = 200;
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(notice(page), 'the next load starts with no stale unsaved flag').toHaveCount(0);
});
