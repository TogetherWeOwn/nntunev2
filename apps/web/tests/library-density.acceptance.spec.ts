import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * DENSITY browser acceptance — protected test-only handoff.
 * Protected files: this spec and src/routes/library/density.acceptance.test.ts.
 * The paired build makes them green without editing them; suspected test
 * defects route back to the test author. Contract: library-density.acceptance.md.
 *
 * Pinned consumer contract (smallest UI, mirroring the appearance preference):
 * - One native select with accessible name "Density" on the library page,
 *   options Comfortable/Compact with raw values `comfortable`/`compact`,
 *   in that order; Comfortable is the server/default choice.
 * - The validated device-local enum lives only under `nntune:density:v1`
 *   (never the appearance key `nntune:appearance:v1`, no cookie/HTTP write).
 *   Unknown, padded, mixed-case, JSON, blank and missing values fall back to
 *   Comfortable. Storage read/write refusal for that key is fail-soft:
 *   session-only choice, no error banner, no throw.
 * - Compact measurably reduces library-row and queue-row height on the
 *   library page (padding/typography steps only): title/artist text stays
 *   visible, WCAG 2.5.8 AA target size holds in both densities (24x24 CSS
 *   px or the spacing exception) and Compact never shrinks a target, Tab
 *   order and visible focus outlines are unchanged, across
 *   find/order/favorites use. Measured baseline note: existing queue-row
 *   action buttons render 21px tall and pass 2.5.8 only via spacing, so an
 *   absolute-24 pin would fail for a pre-existing reason; the build may
 *   still raise them, and these cases accept either outcome.
 * - No class name, attribute, module or storage-format beyond the raw enum
 *   above is prescribed. Compactness is judged by rendered row geometry.
 *
 * RED at the pre-feature base is assertion-level (the Density control is
 * absent) after the real mocked library hydrates; DEN-00 is a passing
 * control proving the harness, rows, queue and existing controls work.
 * No RED-only ready PR: the build continues this branch in one GREEN PR.
 *
 * Offline setup: pnpm --dir apps/web install --offline --frozen-lockfile --prod=false
 * Run: CI=1 pnpm --dir apps/web exec playwright test \
 *        tests/library-density.acceptance.spec.ts --workers=1
 * Chromium must already be installed; some hosts need LD_LIBRARY_PATH
 * pointing at their provisioned browser libraries. Port 4173 must be free
 * (CI=1 refuses reusing a different preview). All network is loopback-only;
 * no backend, provider, staging or production is contacted.
 *
 * Breaks caught: absent/mislabeled control; density applied to only one of
 * the two lists; hidden or shrunken-away title/artist text; sub-24px
 * targets; changed keyboard order or invisible focus; unvalidated or
 * non-enum persistence; storage-failure banners/throws; density lost
 * across find/order/favorites; appearance preference disturbed.
 */

const DENSITY_KEY = 'nntune:density:v1';
const APPEARANCE_KEY = 'nntune:appearance:v1';
const ITEMS = [
	{ id: 'den-zulu', title: 'Zulu Waves', artist: 'The Densities' },
	{ id: 'den-alpha', title: 'Alpha Lines', artist: 'Compact Club' },
	{ id: 'den-mango', title: 'Mango Steps', artist: 'Padding Parade' },
	{ id: 'den-beta', title: 'Beta Rows', artist: 'Comfort Zone' }
];
const IDS = ITEMS.map((item) => item.id);

function density(page: Page): Locator {
	return page.getByRole('combobox', { name: 'Density', exact: true });
}
function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}
function queueRow(page: Page, id: string): Locator {
	return page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);
}

async function expectDensityControl(page: Page, value: 'comfortable' | 'compact') {
	await expect(density(page), 'library page must expose a native select named "Density"').toHaveCount(1);
	await expect(density(page)).toBeVisible();
	await expect(density(page).getByRole('option')).toHaveText(['Comfortable', 'Compact']);
	await expect.poll(() => density(page).locator('option').evaluateAll(
		(options) => options.map((option) => (option as HTMLOptionElement).value)
	)).toEqual(['comfortable', 'compact']);
	await expect(density(page)).toHaveValue(value);
}

async function chooseDensity(page: Page, value: 'comfortable' | 'compact') {
	await expect(density(page), 'library page must expose a native select named "Density"').toHaveCount(1);
	await density(page).selectOption(value);
	await expect(density(page)).toHaveValue(value);
}

async function rowHeight(locator: Locator): Promise<number> {
	return locator.evaluate((element) => element.getBoundingClientRect().height);
}

async function expectRowHeightBelow(locator: Locator, ceiling: number, message: string) {
	await expect.poll(() => rowHeight(locator), { timeout: 2000, message }).toBeLessThanOrEqual(ceiling - 1);
}

async function textVisiblyRendered(locator: Locator, text: string): Promise<boolean> {
	return locator.evaluate((element, needle) => {
		const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
		for (let node = walker.nextNode(); node; node = walker.nextNode()) {
			if (!node.textContent?.includes(needle)) continue;
			const parent = node.parentElement;
			if (!parent?.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true })) continue;
			const range = document.createRange();
			range.selectNodeContents(node);
			const rect = range.getBoundingClientRect();
			if (rect.width > 0 && rect.height >= 6) return true;
		}
		return false;
	}, text);
}

async function expectLibraryTextsVisible(page: Page) {
	for (const item of ITEMS) {
		expect(await textVisiblyRendered(row(page, item.id), item.title),
			`title "${item.title}" must stay visibly rendered`).toBe(true);
		expect(await textVisiblyRendered(row(page, item.id), item.artist),
			`artist "${item.artist}" must stay visibly rendered`).toBe(true);
	}
}

async function expectRowTextsVisible(page: Page) {
	await expectLibraryTextsVisible(page);
	for (const item of [ITEMS[0]!, ITEMS[2]!]) {
		expect(await textVisiblyRendered(queueRow(page, item.id), item.title),
			`queue title "${item.title}" must stay visibly rendered`).toBe(true);
	}
}

/** Visible interactive targets inside the rows (and each row itself when it
 * is the control), with rendered geometry for WCAG 2.5.8 checks. */
async function rowTargets(page: Page) {
	return page.evaluate(() => {
		const interactive = 'button, a[href], input, select, textarea, [role="button"], [tabindex]';
		const elements = new Set<Element>();
		const rows = document.querySelectorAll('[data-testid="library-item"], [data-testid="queue-item"], [data-testid="favorite-item"]');
		for (const rowElement of rows) {
			if (rowElement.matches(interactive)) elements.add(rowElement);
			for (const target of rowElement.querySelectorAll(interactive)) elements.add(target);
		}
		return [...elements].flatMap((element, index) => {
			// A [tabindex="-1"] row is a programmatic-focus landing spot, not an
			// interactive target; its height is the row height DEN-02 expects to
			// change, so it must not enter the control target checks.
			const nativeControl = 'button, a[href], input, select, textarea, [role="button"]';
			if (element.getAttribute('tabindex') === '-1' && !element.matches(nativeControl)) return [];
			if (!(element as HTMLElement).checkVisibility({ checkOpacity: true, checkVisibilityCSS: true })) return [];
			const rect = element.getBoundingClientRect();
			if (rect.width === 0 || rect.height === 0) return [];
			const owner = element.closest('[data-track-id], [data-id]');
			const ownerId = owner?.getAttribute('data-track-id') ?? owner?.getAttribute('data-id') ?? '';
			// The same track renders in several lists (browse, queue, favorites
			// shelf) with identical tag/id/name; the owning list is part of a
			// target's identity, or cross-list comparisons alias distinct buttons.
			const list = element.closest('[data-testid="library-item"], [data-testid="queue-item"], [data-testid="favorite-item"]');
			const listId = list?.getAttribute('data-testid') ?? '';
			const name = element.getAttribute('data-testid') ?? element.getAttribute('aria-label') ?? (element.textContent ?? '').trim().slice(0, 32);
			// Ancestor/descendant pairs (a row button wrapping its star) are one
			// composite target for spacing purposes; record the chain as indices
			// of element identity via a path of tags is unreliable, so record
			// whether each element contains another measured element instead.
			return [{
				index,
				key: `${listId}|${element.tagName.toLowerCase()}|${ownerId}|${name}`,
				x: rect.x, y: rect.y, width: rect.width, height: rect.height,
				contains: [...elements].filter((other) => other !== element && element.contains(other))
					.map((other) => [...elements].indexOf(other))
			}];
		});
	});
}

type RowTarget = Awaited<ReturnType<typeof rowTargets>>[number];

/** WCAG 2.5.8 AA: a target needs 24x24 CSS px, unless a 24px-diameter circle
 * centered on it intersects no other target (or undersized target's circle). */
function targetSizeViolations(targets: RowTarget[]): string[] {
	const related = (a: RowTarget, b: RowTarget) => a.contains.includes(b.index) || b.contains.includes(a.index);
	const circleHitsRect = (cx: number, cy: number, rect: RowTarget) => {
		const dx = Math.max(rect.x - cx, 0, cx - (rect.x + rect.width));
		const dy = Math.max(rect.y - cy, 0, cy - (rect.y + rect.height));
		return dx * dx + dy * dy < 12 * 12;
	};
	const violations: string[] = [];
	for (const target of targets) {
		if (target.width >= 24 && target.height >= 24) continue;
		const cx = target.x + target.width / 2;
		const cy = target.y + target.height / 2;
		const blocked = targets.some((other) => {
			if (other === target || related(target, other)) return false;
			if (other.width < 24 || other.height < 24) {
				const ox = other.x + other.width / 2;
				const oy = other.y + other.height / 2;
				return (cx - ox) ** 2 + (cy - oy) ** 2 < 24 * 24;
			}
			return circleHitsRect(cx, cy, other);
		});
		if (blocked) violations.push(`${target.key} ${Math.round(target.width)}x${Math.round(target.height)}`);
	}
	return violations;
}

async function tabSequence(page: Page): Promise<string[]> {
	// Pin a common sequential-focus origin. Blurring alone is not enough:
	// Chromium retains the sequential focus-navigation starting point from
	// the last click/focus, so two captures would be rotations of the same
	// order rather than the same sequence. Focusing the Density select
	// explicitly before each capture makes both start at the same anchor;
	// blur first so the anchor focus is a real focus change in both captures
	// (focusing the already-focused select is a no-op that leaves the
	// previous starting point in place).
	await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
	await density(page).focus();
	await expect(density(page)).toBeFocused();
	const sequence: string[] = [];
	for (let step = 0; step < 60; step += 1) {
		await page.keyboard.press('Tab');
		const descriptor = await page.evaluate(() => {
			const element = document.activeElement;
			if (!element || element === document.body) return 'body';
			const owner = element.closest('[data-track-id], [data-id]');
			const ownerId = owner?.getAttribute('data-track-id') ?? owner?.getAttribute('data-id') ?? '';
			const list = element.closest('[data-testid="library-item"], [data-testid="queue-item"], [data-testid="favorite-item"]');
			const listId = list?.getAttribute('data-testid') ?? '';
			return `${listId}|${element.tagName.toLowerCase()}|${element.getAttribute('data-testid') ?? ''}|${ownerId}|${element.getAttribute('aria-label') ?? ''}|${(element.textContent ?? '').trim().slice(0, 24)}`;
		});
		if (descriptor === 'body' && sequence.length > 0) break;
		if (sequence.includes(descriptor)) break;
		sequence.push(descriptor);
	}
	return sequence;
}

async function openLibrary(page: Page) {
	const writes: string[] = [];
	page.on('request', (request) => {
		if (!['GET', 'HEAD'].includes(request.method())) writes.push(`${request.method()} ${new URL(request.url()).pathname}`);
	});
	await page.route('**/api/library', (route) => route.fulfill({
		status: 200, contentType: 'application/json', body: JSON.stringify(ITEMS)
	}));
	await page.goto('/library');
	await expect.poll(() => page.getByTestId('library-item').evaluateAll(
		(rows) => rows.map((item) => item.getAttribute('data-id'))
	)).toEqual(IDS);
	return { writes };
}

async function queueTwo(page: Page) {
	await row(page, 'den-zulu').click();
	await row(page, 'den-mango').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
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

test('DEN-00 control: real page, rows, queue, find/order/appearance controls and WCAG target sizes already work', async ({ page }) => {
	const { writes } = await openLibrary(page);
	await queueTwo(page);
	await expectRowTextsVisible(page);
	await expect(page.getByRole('searchbox', { name: /find|search/i })
		.or(page.getByRole('textbox', { name: /find|search/i }))).toBeVisible();
	await expect(page.getByRole('combobox', { name: /order|sort/i })).toBeVisible();
	await expect(page.getByRole('combobox', { name: 'Appearance', exact: true })).toBeVisible();
	expect(targetSizeViolations(await rowTargets(page)),
		'existing row targets must already satisfy WCAG 2.5.8 (24x24 or spacing)').toEqual([]);
	expect(writes).toEqual([]);
});

test('DEN-01 default: Density control offers Comfortable/Compact and starts at Comfortable', async ({ page }) => {
	await openLibrary(page);
	await expectDensityControl(page, 'comfortable');
	await expect(page.getByText('Density', { exact: true })).toBeVisible();
	const box = await density(page).boundingBox();
	expect(box, 'Density select must have a layout box').not.toBeNull();
	expect(box!.height, 'Density select height must be at least 24px').toBeGreaterThanOrEqual(24);
	expect(box!.width, 'Density select width must be at least 24px').toBeGreaterThanOrEqual(24);
});

test('DEN-02 compact: both lists shrink, text stays visible, Comfortable restores the geometry', async ({ page }) => {
	await openLibrary(page);
	await queueTwo(page);
	const comfortableLibrary = await rowHeight(row(page, 'den-alpha'));
	const comfortableQueue = await rowHeight(queueRow(page, 'den-zulu'));

	await chooseDensity(page, 'compact');
	await expectRowHeightBelow(row(page, 'den-alpha'), comfortableLibrary, 'library rows must get shorter in Compact');
	await expectRowHeightBelow(queueRow(page, 'den-zulu'), comfortableQueue, 'queue rows must get shorter in Compact');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expectRowTextsVisible(page);

	await chooseDensity(page, 'comfortable');
	await expect.poll(() => rowHeight(row(page, 'den-alpha')), { timeout: 2000, message: 'library rows must return to Comfortable height' })
		.toBeGreaterThanOrEqual(comfortableLibrary - 0.5);
	await expect.poll(() => rowHeight(queueRow(page, 'den-zulu')), { timeout: 2000, message: 'queue rows must return to Comfortable height' })
		.toBeGreaterThanOrEqual(comfortableQueue - 0.5);
	await expectRowTextsVisible(page);
});

test('DEN-03 targets: WCAG 2.5.8 target size holds in both densities and Compact shrinks no target', async ({ page }) => {
	await openLibrary(page);
	await queueTwo(page);
	await row(page, 'den-beta').getByRole('button', { name: /star|favorite/i }).click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	const comfortable = await rowTargets(page);
	expect(targetSizeViolations(comfortable), 'Comfortable targets').toEqual([]);

	await chooseDensity(page, 'compact');
	const compact = await rowTargets(page);
	expect(targetSizeViolations(compact), 'Compact targets').toEqual([]);
	const comfortableByKey = new Map(comfortable.map((target) => [target.key, target]));
	for (const target of compact) {
		const before = comfortableByKey.get(target.key);
		expect(before, `Compact must not add or remove row targets (${target.key})`).toBeDefined();
		expect(target.width, `${target.key} width must not shrink in Compact`).toBeGreaterThanOrEqual(before!.width - 0.5);
		expect(target.height, `${target.key} height must not shrink in Compact`).toBeGreaterThanOrEqual(before!.height - 0.5);
	}
	expect(compact).toHaveLength(comfortable.length);

	await chooseDensity(page, 'comfortable');
	expect(targetSizeViolations(await rowTargets(page)), 'restored Comfortable targets').toEqual([]);
});

test('DEN-04 keyboard: Tab order and visible focus are identical in both densities; the select works by keyboard', async ({ page }) => {
	await openLibrary(page);
	await queueTwo(page);
	await expectDensityControl(page, 'comfortable');
	const comfortableSequence = await tabSequence(page);
	expect(comfortableSequence.length).toBeGreaterThan(5);

	// Reach and operate the Density select through the real tab order.
	await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
	for (let tabs = 0; tabs < 60 && !(await density(page).evaluate((element) => element === document.activeElement)); tabs += 1) {
		await page.keyboard.press('Tab');
	}
	await expect(density(page)).toBeFocused();
	await page.keyboard.press('Home');
	await page.keyboard.press('ArrowDown');
	await page.keyboard.press('Enter');
	await expect(density(page)).toHaveValue('compact');
	await expect(density(page)).toBeFocused();
	const outline = await density(page).evaluate((element) => {
		const css = getComputedStyle(element);
		return { style: css.outlineStyle, width: parseFloat(css.outlineWidth) };
	});
	expect(outline.style, 'focused Density select must keep a visible outline in Compact').not.toBe('none');
	expect(outline.width).toBeGreaterThan(0);

	const compactSequence = await tabSequence(page);
	expect(compactSequence, 'Tab order must not change with density').toEqual(comfortableSequence);
});

test('DEN-05 persistence: choice stores only the raw enum, survives reload both ways, and never writes the appearance key', async ({ page }) => {
	await page.addInitScript(() => {
		const audit = { writes: [] as [string, string][] };
		(window as Window & { __densityAudit?: typeof audit }).__densityAudit = audit;
		const setItem = Storage.prototype.setItem;
		Storage.prototype.setItem = function (key, value) {
			audit.writes.push([String(key), String(value)]);
			return setItem.call(this, key, value);
		};
	});
	const { writes } = await openLibrary(page);
	// addInitScript re-runs on every navigation and resets the in-page audit,
	// so accumulate writes outside the page realm: collect each realm's audit
	// before reloading destroys it (a reload-time rewrite would still land in
	// the next realm's fragment and be caught below).
	const storageWrites: [string, string][] = [];
	const collectAudit = async () => {
		storageWrites.push(...await page.evaluate(() =>
			(window as Window & { __densityAudit: { writes: [string, string][] } }).__densityAudit.writes));
	};
	await queueTwo(page);
	const comfortable = await rowHeight(row(page, 'den-alpha'));
	await page.evaluate((key) => localStorage.setItem(key, 'dark'), APPEARANCE_KEY);
	await page.evaluate(() => {
		(window as Window & { __densityAudit: { writes: [string, string][] } }).__densityAudit.writes = [];
	});

	await chooseDensity(page, 'compact');
	expect(await page.evaluate((key) => localStorage.getItem(key), DENSITY_KEY)).toBe('compact');
	await collectAudit();
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expectDensityControl(page, 'compact');
	await expectRowHeightBelow(row(page, 'den-alpha'), comfortable, 'Compact must still apply after reload');

	await chooseDensity(page, 'comfortable');
	expect(await page.evaluate((key) => localStorage.getItem(key), DENSITY_KEY)).toBe('comfortable');
	await collectAudit();
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expectDensityControl(page, 'comfortable');
	await expect.poll(() => rowHeight(row(page, 'den-alpha'))).toBeGreaterThanOrEqual(comfortable - 0.5);

	await collectAudit();
	expect(storageWrites.filter(([key]) => key === DENSITY_KEY || key === APPEARANCE_KEY),
		'density changes must write only the density enum').toEqual([
		[DENSITY_KEY, 'compact'], [DENSITY_KEY, 'comfortable']
	]);
	expect(await page.evaluate((key) => localStorage.getItem(key), APPEARANCE_KEY)).toBe('dark');
	expect(writes, 'density must not become an HTTP write').toEqual([]);
});

const INVALID = [
	['unknown', 'cozy'],
	['case', 'Compact'],
	['padded', ' compact '],
	['json-string', '"compact"'],
	['json-object', '{"density":"compact"}'],
	['empty', '']
] as const;
for (const [id, raw] of INVALID) {
	test(`DEN-06-${id}: invalid stored value ${JSON.stringify(raw)} falls back to Comfortable`, async ({ page }) => {
		await page.addInitScript(({ key, value }) => localStorage.setItem(key, value), { key: DENSITY_KEY, value: raw });
		await openLibrary(page);
		await expectDensityControl(page, 'comfortable');
		await expectLibraryTextsVisible(page);
	});
}

for (const refusal of ['read', 'write'] as const) {
	test(`DEN-07-${refusal}: storage ${refusal} refusal is fail-soft and session-only with no error banner`, async ({ page }) => {
		const pageErrors: string[] = [];
		page.on('pageerror', (error) => pageErrors.push(error.message));
		await page.addInitScript(({ key, mode }) => {
			if (mode === 'read') {
				const getItem = Storage.prototype.getItem;
				Storage.prototype.getItem = function (name) {
					if (name === key) throw new DOMException('Storage unavailable', 'SecurityError');
					return getItem.call(this, name);
				};
			} else {
				const setItem = Storage.prototype.setItem;
				Storage.prototype.setItem = function (name, value) {
					if (name === key) throw new DOMException('Storage unavailable', 'QuotaExceededError');
					return setItem.call(this, name, value);
				};
			}
		}, { key: DENSITY_KEY, mode: refusal });
		await openLibrary(page);
		await queueTwo(page);
		await expectDensityControl(page, 'comfortable');
		const comfortable = await rowHeight(row(page, 'den-alpha'));

		await chooseDensity(page, 'compact');
		await expectRowHeightBelow(row(page, 'den-alpha'), comfortable, 'in-memory Compact must still apply when storage refuses');
		await expectRowTextsVisible(page);
		await expect(page.getByRole('alert'), 'storage refusal must not raise an error banner').toHaveCount(0);
		await expect(page.getByRole('main')).toBeVisible();

		await page.reload();
		await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
		await expectDensityControl(page, 'comfortable');
		expect(pageErrors, 'no uncaught errors from storage refusal').toEqual([]);
	});
}

test('DEN-08 views: Compact persists across find, ordering and favorites use on the library page', async ({ page }) => {
	await openLibrary(page);
	await queueTwo(page);
	const comfortableLibrary = await rowHeight(row(page, 'den-alpha'));
	const comfortableQueue = await rowHeight(queueRow(page, 'den-zulu'));
	await chooseDensity(page, 'compact');
	await expectRowHeightBelow(row(page, 'den-alpha'), comfortableLibrary, 'Compact applies before view changes');

	// Find: filtered rows stay compact and readable.
	const find = page.getByRole('searchbox', { name: /find|search/i })
		.or(page.getByRole('textbox', { name: /find|search/i }));
	await find.fill('Alpha');
	await expect(row(page, 'den-beta')).toBeHidden();
	await expect(row(page, 'den-alpha')).toBeVisible();
	await expectRowHeightBelow(row(page, 'den-alpha'), comfortableLibrary, 'found rows must stay Compact');
	expect(await textVisiblyRendered(row(page, 'den-alpha'), 'Alpha Lines')).toBe(true);
	await find.fill('');

	// Order: reordered rows stay compact.
	const ordering = page.getByRole('combobox', { name: /order|sort/i });
	await ordering.selectOption({ label: 'Title A–Z' });
	await expect.poll(() => page.getByTestId('library-item').first().getAttribute('data-id')).toBe('den-alpha');
	await expectRowHeightBelow(row(page, 'den-alpha'), comfortableLibrary, 'reordered rows must stay Compact');

	// Favorites: starring works, favorite text/targets stay usable, and the
	// browse + queue lists keep Compact. (Whether favorite rows themselves
	// compact is deliberately not pinned; see the handoff note.)
	await row(page, 'den-beta').getByRole('button', { name: /star|favorite/i }).click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	expect(await textVisiblyRendered(page.getByTestId('favorite-item').first(), 'Beta Rows')).toBe(true);
	expect(targetSizeViolations(await rowTargets(page)), 'WCAG target size holds after favorites use').toEqual([]);
	await expectRowHeightBelow(row(page, 'den-alpha'), comfortableLibrary, 'browse rows must stay Compact after starring');
	await expectRowHeightBelow(queueRow(page, 'den-zulu'), comfortableQueue, 'queue rows must stay Compact after starring');
	await expect(density(page)).toHaveValue('compact');
});
