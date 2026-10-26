import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Builder-owned focus pins complementing the protected
 * `library-find-scope.spec.ts` bank (its FS-B03 covers counts, zero-match
 * copy and text-preserving recovery, but not where focus lands). This
 * file is safe for the Builder to extend; the bank stays byte-identical.
 *
 * The "Search both title and artist" recovery button only exists under a
 * narrowed scope, so activating it unmounts the very button that had
 * focus. Focus must land on the "Search in" scope selector — the control
 * whose value just changed back to Both — never drop to <body>. This
 * mirrors the shipped clearFind (focus returns to the find field) and
 * showAllTracks (focus returns to the view selector) handling.
 *
 * Offline: the library API is mocked; no backend/prod/provider traffic.
 * Run: CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *        tests/library-find-scope-focus-builder.spec.ts --workers=1 --retries=0
 */

const ITEMS = [
	{ id: 'focus-alpha', title: 'Harbor Song', artist: 'Echo' },
	{ id: 'focus-beta', title: 'Morning Song', artist: 'Harbor Lights' },
	{ id: 'focus-gamma', title: 'Gamma Tune', artist: 'Echo' }
];

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

function findField(page: Page): Locator {
	return page.getByLabel('Find in loaded tracks');
}

function scopeSelect(page: Page): Locator {
	return page.getByLabel('Search in');
}

function recoveryButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Search both title and artist', exact: true });
}

test('[FS-FOCUS-01] keyboard recovery from a title-only zero match lands focus on the scope selector', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = findField(page);
	const scope = scopeSelect(page);

	await field.fill('echo');
	await scope.selectOption({ label: 'Title only' });
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await expect(recoveryButton(page)).toBeVisible();

	await recoveryButton(page).focus();
	await expect(recoveryButton(page)).toBeFocused();
	await page.keyboard.press('Enter');

	// The activated button unmounts with the narrowed scope; focus must be
	// restored to the still-mounted scope selector, not dropped to <body>.
	await expect(scope).toBeFocused();
	await expect(scope).toHaveValue('both');
	await expect(field, 'recovery must not lose the typed text').toHaveValue('echo');
	await expectRows(page, ['focus-alpha', 'focus-gamma']);
});

test('[FS-FOCUS-02] keyboard recovery from an artist-only zero match lands focus on the scope selector', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = findField(page);
	const scope = scopeSelect(page);

	await field.fill('gamma');
	await scope.selectOption({ label: 'Artist only' });
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await expect(recoveryButton(page)).toBeVisible();

	await recoveryButton(page).focus();
	await expect(recoveryButton(page)).toBeFocused();
	await page.keyboard.press('Enter');

	await expect(scope).toBeFocused();
	await expect(scope).toHaveValue('both');
	await expect(field, 'recovery must not lose the typed text').toHaveValue('gamma');
	await expectRows(page, ['focus-gamma']);
});
