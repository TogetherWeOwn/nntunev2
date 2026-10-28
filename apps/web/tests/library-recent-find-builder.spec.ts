import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Builder-owned review-round pins for recent library find recall.
 * The paired acceptance bank (`library-recent-find.spec.ts`) stays
 * byte-identical; this file covers review-found behavior the bank does not
 * pin: chip clicks with a pending unrecorded query, pagehide/unmount
 * recording, focus after clearing, and the 120-char bound with its
 * 140-char announcement cap.
 */

const ITEMS = [
	{ id: 'rec-alpha', title: 'Harbor Song', artist: 'Echo' },
	{ id: 'rec-beta', title: 'Beta Song', artist: 'Harbor Lights' },
	{ id: 'rec-gamma', title: 'Gamma Tune', artist: 'Echo' },
	{ id: 'rec-untitled' },
	{ id: 'rec-delta', title: 'Delta Song', artist: '   ' },
	{ id: 'rec-regex', title: 'What.*Ever', artist: 'C++ Players' },
	{ id: 'rec-accent', title: 'Éclair', artist: 'écho' },
	{ id: 'rec-zulu', title: 'Zulu Last', artist: 'Zulu' }
];

const API_IDS = ITEMS.map((item) => item.id);

const AUTH_CONTEXT_KEY = 'nntune.library.auth-context.v1';
const RECENT_BASE = 'nntune.library.recent-finds.v1';

async function mockLibrary(page: Page) {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(ITEMS)
		})
	);
}

async function openLibrary(page: Page) {
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expectRows(page, API_IDS);
}

async function expectRows(page: Page, ids: string[]) {
	await expect
		.poll(() =>
			page
				.getByTestId('library-item')
				.evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')))
		)
		.toEqual(ids);
}

async function requireFind(page: Page): Promise<Locator> {
	const field = page
		.getByRole('searchbox', { name: /find|search/i })
		.or(page.getByRole('textbox', { name: /find|search/i }));
	await expect(field).toBeVisible();
	return field;
}

function recentChips(page: Page): Locator {
	return page.getByTestId('recent-find-chip');
}

function recentEmpty(page: Page): Locator {
	return page.getByTestId('recent-finds-empty');
}

function recentAnnouncement(page: Page): Locator {
	return page.getByTestId('recent-finds-announcement');
}

function clearRecents(page: Page): Locator {
	return page.getByRole('button', { name: 'Clear recent searches', exact: true });
}

function recentChip(page: Page, query: string): Locator {
	return page.getByTestId('recent-finds').getByRole('button', { name: query, exact: true });
}

async function chipNames(page: Page): Promise<string[]> {
	return recentChips(page).evaluateAll((els) =>
		els.map((el) => (el.getAttribute('aria-label') ?? el.textContent ?? '').trim())
	);
}

async function expectChips(page: Page, expected: string[]) {
	await expect
		.poll(() => chipNames(page), {
			message: `recent chips must be [${expected.join(', ')}] most-recent-first`
		})
		.toEqual(expected);
}

async function recordViaBlur(page: Page, field: Locator, query: string) {
	await field.fill(query);
	await expect(field).toHaveValue(query);
	await field.blur();
}

async function readRecentRaw(page: Page): Promise<string | null> {
	return page.evaluate(
		({ base, contextKey }) => {
			const context = window.sessionStorage.getItem(contextKey);
			return context ? window.localStorage.getItem(`${base}.${context}`) : null;
		},
		{ base: RECENT_BASE, contextKey: AUTH_CONTEXT_KEY }
	);
}

function storedQueries(raw: string | null): string[] {
	if (!raw) return [];
	try {
		const parsed: unknown = JSON.parse(raw);
		if (!Array.isArray(parsed)) return [];
		return parsed.filter(
			(value): value is string => typeof value === 'string' && value.length > 0
		);
	} catch {
		return [];
	}
}

test.beforeEach(async ({ context }) => {
	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/')) {
			return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		}
		return route.continue();
	});
});

test('[RF-B1] chip click with a pending query applies the chip and keeps the pending query', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await recordViaBlur(page, field, 'beta');
	await recordViaBlur(page, field, 'harbor');
	await expectChips(page, ['harbor', 'beta']);

	// A pending unrecorded query with the field still focused: the click
	// must land on its chip instead of shifting under the pointer.
	await field.fill('song');
	await expectRows(page, ['rec-alpha', 'rec-beta', 'rec-delta']);
	await recentChip(page, 'beta').click();
	await expect(field).toHaveValue('beta');
	await expectRows(page, ['rec-beta']);
	await expectChips(page, ['beta', 'song', 'harbor']);
});

test('[RF-B2] pagehide records the applied query without a blur', async ({ page }) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await recordViaBlur(page, field, 'beta');
	await expectChips(page, ['beta']);

	await field.fill('gamma');
	await expectRows(page, ['rec-gamma']);
	await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pagehide')));
	await expectChips(page, ['gamma', 'beta']);
	expect(storedQueries(await readRecentRaw(page))).toEqual(['gamma', 'beta']);
});

test('[RF-B3] client-side navigation away records the applied query without a blur', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await recordViaBlur(page, field, 'beta');
	await expectChips(page, ['beta']);

	// Apply a query without ever focusing the field, so no blur precedes
	// the navigation and only the unmount recorder can fire.
	await page.evaluate(() => {
		const el = document.getElementById('library-find');
		if (!(el instanceof HTMLInputElement)) throw new Error('find field missing');
		el.value = 'zulu';
		el.dispatchEvent(new Event('input', { bubbles: true }));
	});
	await expectRows(page, ['rec-zulu']);
	expect(await page.evaluate(() => document.activeElement === document.body)).toBe(true);

	await page.getByRole('link', { name: 'Select NZB locally' }).click();
	await expect(page).toHaveURL(/\/library\/import/);
	expect(storedQueries(await readRecentRaw(page))[0]).toBe('zulu');

	await page.getByRole('link', { name: 'Library', exact: true }).click();
	await expect(page).toHaveURL(/\/library$/);
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expectChips(page, ['zulu', 'beta']);
});

test('[RF-B4] clearing keeps focus in the section instead of dropping to body', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await recordViaBlur(page, field, 'beta');
	await expectChips(page, ['beta']);

	await clearRecents(page).focus();
	await page.keyboard.press('Enter');
	await expect(recentChips(page)).toHaveCount(0);
	await expect(recentEmpty(page)).toBeVisible();
	await expect(recentEmpty(page)).toBeFocused();
	expect(await page.evaluate(() => document.activeElement === document.body)).toBe(false);
});

test('[RF-B5] a 120-char query records with a brief announcement; 121 chars never records', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	const allowed = 'q'.repeat(120);
	await field.fill(allowed);
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await field.blur();
	await expectChips(page, [allowed]);
	const text = ((await recentAnnouncement(page).textContent()) ?? '').trim();
	expect(text, 'recording a max-length query must announce').not.toBe('');
	expect(text.length, 'announcement must stay within 140 chars').toBeLessThanOrEqual(140);

	// Re-applying the max-length chip announces briefly too.
	await recentChip(page, allowed).click();
	await expect(field).toHaveValue(allowed);
	const reapplied = ((await recentAnnouncement(page).textContent()) ?? '').trim();
	expect(reapplied.length).toBeLessThanOrEqual(140);

	await field.fill('q'.repeat(121));
	await field.blur();
	await expectChips(page, [allowed]);
	expect(storedQueries(await readRecentRaw(page))).toEqual([allowed]);
});
