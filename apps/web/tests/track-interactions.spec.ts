import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page } from '@playwright/test';
import { TESTIDS } from '../../../e2e/helpers/contract';

const ITEMS = [
	{ id: 'coral', title: 'Coral Night', artist: 'Harbor Lights' },
	{ id: 'dawn', title: 'Dawn Chorus' },
	{ id: 'ember', title: 'Ember Sky' },
	{ id: 'tide', title: 'Tide Pools' }
];

const pageErrors: string[] = [];
test.beforeEach(() => { pageErrors.length = 0; });
test.afterEach(() => { expect(pageErrors).toEqual([]); });

async function seed(page: Page, items: Array<{ id: string; title: string; artist?: string; streamUrl?: string }> = ITEMS) {
	page.on('pageerror', (error) => pageErrors.push(error.message));
	await page.route('**/api/library', (route) => route.fulfill({ json: items }));
	await page.goto('/library');
	await expect(page.getByTestId(TESTIDS.libraryItem)).toHaveCount(items.length);
}

const libraryRow = (page: Page, id: string) => page.locator(`[data-testid="library-item"][data-id="${id}"]`);
const queueRow = (page: Page, id: string) => page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);
const menu = (page: Page) => page.getByTestId(TESTIDS.trackContextMenu);

async function startFocusJournal(page: Page) {
	await page.evaluate(() => {
		const entries: object[] = [];
		let sequence = 0;
		const identity = (node: EventTarget | null) => {
			if (node === document) return 'document';
			if (node === window) return 'window';
			if (!(node instanceof Element)) return 'other';
			return node.getAttribute('data-testid') || node.tagName.toLowerCase();
		};
		const record = (type: string, target: EventTarget | null) => {
			if (entries.length >= 250) return;
			const element = target instanceof Element ? target : null;
			entries.push({ sequence: ++sequence, time: performance.now(), type, target: identity(target),
				active: identity(document.activeElement), menu: !!document.querySelector('[data-testid="track-context-menu"]'),
				x: window.scrollX, y: window.scrollY, targetScroll: element && [element.scrollLeft, element.scrollTop] });
		};
		for (const type of ['scroll', 'pointerdown', 'mousedown', 'focusout', 'focusin', 'pointerup', 'mouseup', 'click']) {
			window.addEventListener(type, (event) => record(type, event.target), { capture: true, passive: true });
		}
		new MutationObserver((records) => {
			for (const mutation of records) for (const node of mutation.removedNodes) {
				if (node instanceof Element && (node.matches('[data-testid="track-context-menu"]') || node.querySelector('[data-testid="track-context-menu"]'))) record('menu-removed', node);
			}
		}).observe(document, { childList: true, subtree: true });
		Object.assign(window, { focusJournal: entries, focusCheckpoint: (name: string) => record(name, document.activeElement) });
	});
}

async function focusCheckpoint(page: Page, name: string) {
	await page.evaluate((label) => (window as unknown as { focusCheckpoint: (label: string) => void }).focusCheckpoint(label), name);
}

async function attachFocusJournal(page: Page, testInfo: import('@playwright/test').TestInfo) {
	const journal = await page.evaluate(() => (window as unknown as { focusJournal: object[] }).focusJournal);
	await testInfo.attach('focus-event-journal', { body: JSON.stringify(journal, null, 2), contentType: 'application/json' });
	// CI retains test-result attachments on failure only; keep passing journals in the job log.
	console.log(JSON.stringify({ test: testInfo.title, focusEventJournal: journal }));
}

async function queueAll(page: Page) {
	for (const item of ITEMS) await libraryRow(page, item.id).locator('.nt-library-row').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(ITEMS.length);
}

async function queueIds(page: Page) {
	return page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
}

test('More, right-click and keyboard menus share actions, navigation and return focus', async ({ page }) => {
	await seed(page);
	const more = libraryRow(page, 'coral').getByTestId(TESTIDS.trackMore);
	await more.click();
	await expect(menu(page).getByRole('menuitem', { name: 'Play now', exact: true })).toBeFocused();
	await page.keyboard.press('End');
	await expect(menu(page).getByRole('menuitem', { name: 'Copy link' })).toBeFocused();
	await page.keyboard.press('ArrowUp');
	await expect(menu(page).getByRole('menuitem', { name: 'Add to playlist, not available yet' })).toBeFocused();
	await page.keyboard.press('ArrowUp');
	const star = menu(page).getByRole('menuitem', { name: 'Star', exact: true });
	await expect(star).toBeFocused();
	await expect(star).not.toHaveAttribute('aria-checked');
	await expect(star).toHaveText('Star');
	await page.keyboard.press('Home');
	await page.keyboard.press('a');
	await expect(menu(page).getByRole('menuitem', { name: 'Add to queue', exact: true })).toBeFocused();
	await page.keyboard.press('Escape');
	await expect(menu(page)).toHaveCount(0);
	await expect(more).toBeFocused();
	await more.press('Shift+F10');
	await expect(menu(page)).toBeVisible();
	await page.keyboard.press('Escape');
	await libraryRow(page, 'coral').click({ button: 'right' });
	await expect(menu(page)).toBeVisible();
	await page.keyboard.press('Escape');
	await expect(more).toBeFocused();
	await libraryRow(page, 'coral').click({ button: 'right' });
	await menu(page).getByRole('menuitem', { name: 'Add to queue', exact: true }).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('queue-item')).toHaveAttribute('data-track-id', 'coral');
	await expect(page.locator('audio[src]:not([src=""])')).toHaveCount(0);
});

test('Copy link is inline, restores focus and preserves whole-menu dismissal', async ({ page }, testInfo) => {
	await page.setViewportSize({ width: 1280, height: 1800 });
	await page.addInitScript(() => {
		Object.defineProperty(navigator, 'clipboard', { value: { writeText: async (url: string) => {
			(window as unknown as { copiedLink: string }).copiedLink = url;
		} } });
	});
	await seed(page);
	const more = libraryRow(page, 'coral').getByTestId(TESTIDS.trackMore);
	await more.click();
	await expect(page.getByRole('menuitem', { name: 'Share', exact: true })).toHaveCount(0);
	await page.keyboard.press('End');
	await expect(page.getByRole('menuitem', { name: 'Copy link', exact: true })).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(menu(page)).toHaveCount(0);
	await expect(more).toBeFocused();
	await expect.poll(() => page.evaluate(() => (window as unknown as { copiedLink: string }).copiedLink))
		.toContain('/library?track=coral');
	await more.click();
	await page.keyboard.press('Tab');
	await expect(menu(page)).toHaveCount(0);
	await expect(page.locator('body')).not.toBeFocused();
	const heading = page.getByRole('heading', { name: 'Library', exact: true });
	await heading.scrollIntoViewIfNeeded();
	await more.scrollIntoViewIfNeeded();
	await startFocusJournal(page);
	try {
		await more.click();
		await expect(menu(page).getByRole('menuitem', { name: 'Play now', exact: true })).toBeFocused();
		await expect(heading).toBeInViewport({ ratio: 1 });
		await focusCheckpoint(page, 'before-heading-click');
		await heading.click();
		await focusCheckpoint(page, 'after-heading-click');
		const events = await page.evaluate(() => (window as unknown as { focusJournal: Array<{ type: string; target: string; menu: boolean }> }).focusJournal);
		expect(events.some((event) => event.type === 'pointerdown' && event.target === 'h1' && event.menu)).toBe(true);
		await expect(menu(page)).toHaveCount(0);
		await expect(more).toBeFocused();
	} finally {
		await attachFocusJournal(page, testInfo);
	}
});

for (const scenario of ['outside-click', 'scroll-only', 'scroll-then-click', 'outside-focus', 'scroll-then-focus', 'explicit-focus', 'scroll-then-explicit-focus'] as const) {
	test(`track menu focus contract: ${scenario}`, async ({ page }, testInfo) => {
		await page.setViewportSize({ width: 1280, height: 1000 });
		await seed(page);
		// Ensure a real document scroll is available without changing the controls under test.
		await page.evaluate(() => {
			const spacer = document.createElement('div');
			spacer.dataset.testid = 'focus-scroll-spacer';
			spacer.style.height = '1200px';
			document.body.append(spacer);
		});
		const more = libraryRow(page, 'coral').getByTestId(TESTIDS.trackMore);
		const heading = page.getByRole('heading', { name: 'Library', exact: true });
		const refresh = page.getByRole('button', { name: 'Refresh', exact: true });
		await heading.scrollIntoViewIfNeeded();
		await more.scrollIntoViewIfNeeded();
		await startFocusJournal(page);
		try {
			let baseline: string | null = null;
			let coordinates: { x: number; y: number } | null = null;
			if (scenario === 'scroll-then-click') {
				await page.evaluate(() => window.scrollBy(0, 40));
				await expect.poll(() => page.evaluate(() => window.scrollY)).toBe(40);
				await more.evaluate((node: HTMLElement) => node.focus({ preventScroll: true }));
				await expect(more).toBeFocused();
				expect(await page.evaluate(() => window.scrollY)).toBe(40);
				const box = await heading.boundingBox();
				expect(box).not.toBeNull();
				coordinates = { x: box!.x + box!.width / 2, y: box!.y + box!.height / 2 };
				await focusCheckpoint(page, 'baseline-before-click');
				await page.mouse.click(coordinates.x, coordinates.y);
				baseline = await page.evaluate(() => document.activeElement?.getAttribute('data-testid') || document.activeElement?.tagName || null);
				await focusCheckpoint(page, 'baseline-after-click');
				await page.evaluate(() => window.scrollTo(0, 0));
				await expect.poll(() => page.evaluate(() => window.scrollY)).toBe(0);
			}
			await more.click();
			await expect(menu(page)).toBeVisible();
			await expect(menu(page).getByRole('menuitem', { name: 'Play now', exact: true })).toBeFocused();
			await focusCheckpoint(page, 'before-stimulus');
			const journalStart = await page.evaluate(() => (window as unknown as { focusJournal: object[] }).focusJournal.length);
			if (scenario.startsWith('scroll-')) {
				const before = await page.evaluate(() => window.scrollY);
				await page.evaluate(() => window.scrollBy(0, 40));
				await expect.poll(() => page.evaluate(() => window.scrollY)).toBe(before + 40);
				await expect(menu(page)).toHaveCount(0);
				await expect(more).toBeFocused();
				expect(await page.evaluate(() => window.scrollY)).toBe(before + 40);
				const scrollEvents = await page.evaluate((start) => (window as unknown as { focusJournal: Array<{ type: string; target: string; menu: boolean }> }).focusJournal.slice(start), journalStart);
				expect(scrollEvents.some((event) => event.type === 'scroll' && event.target === 'document' && event.menu)).toBe(true);
				await focusCheckpoint(page, 'after-scroll-dismissal');
			}
			if (scenario === 'outside-click') {
				const before = await page.evaluate(() => window.scrollY);
				const box = await heading.boundingBox();
				expect(box).not.toBeNull();
				await page.mouse.click(box!.x + box!.width / 2, box!.y + box!.height / 2);
				await expect(menu(page)).toHaveCount(0);
				await expect(more).toBeFocused();
				expect(await page.evaluate(() => window.scrollY)).toBe(before);
				const events = await page.evaluate((start) => (window as unknown as { focusJournal: Array<{ type: string; target: string }> }).focusJournal.slice(start), journalStart);
				expect(events.filter((event) => event.type === 'scroll' || (event.type === 'focusin' && event.target !== 'track-more'))).toEqual([]);
			} else if (scenario === 'scroll-then-click') {
				const box = await heading.boundingBox();
				expect(box).not.toBeNull();
				expect({ x: box!.x + box!.width / 2, y: box!.y + box!.height / 2 }).toEqual(coordinates);
				await page.mouse.click(coordinates!.x, coordinates!.y);
				expect(await page.evaluate(() => document.activeElement?.getAttribute('data-testid') || document.activeElement?.tagName || null)).toBe(baseline);
				await expect(menu(page)).toHaveCount(0);
			} else if (scenario.endsWith('explicit-focus')) {
				const before = await page.evaluate(() => window.scrollY);
				const focusStart = await page.evaluate(() => (window as unknown as { focusJournal: object[] }).focusJournal.length);
				await refresh.evaluate((node: HTMLElement) => node.focus({ preventScroll: true }));
				await expect(menu(page)).toHaveCount(0);
				await expect(refresh).toBeFocused();
				expect(await page.evaluate(() => window.scrollY)).toBe(before);
				const events = await page.evaluate((start) => (window as unknown as { focusJournal: Array<{ type: string }> }).focusJournal.slice(start), focusStart);
				expect(events.some((event) => event.type === 'focusin')).toBe(true);
				expect(events.filter((event) => ['scroll', 'pointerdown', 'mousedown', 'pointerup', 'mouseup', 'click'].includes(event.type))).toEqual([]);
			} else if (scenario.endsWith('focus')) {
				await refresh.click();
				await expect(menu(page)).toHaveCount(0);
				await expect(refresh).toBeFocused();
			}
			await focusCheckpoint(page, 'after-stimulus');
		} finally {
			await attachFocusJournal(page, testInfo);
		}
	});
}

test('reserved playlist slots are discoverable but never activate or persist', async ({ page }) => {
	await seed(page);
	await queueAll(page);
	const requests: string[] = [];
	page.on('request', (request) => requests.push(request.url()));
	const snapshot = () => page.evaluate(() => ({
		queue: [...document.querySelectorAll('[data-testid="queue-item"]')].map((row) => row.getAttribute('data-track-id')),
		current: document.querySelector('[data-testid="now-playing"]')?.textContent,
		local: { ...localStorage }, session: { ...sessionStorage }
	}));
	for (const more of [libraryRow(page, 'coral').getByTestId(TESTIDS.trackMore), queueRow(page, 'coral').getByTestId(TESTIDS.queueMore)]) {
		const before = await snapshot();
		await more.click();
		const slot = menu(page).getByRole('menuitem', { name: 'Add to playlist, not available yet', exact: true });
		await expect(slot).toHaveAttribute('aria-disabled', 'true');
		await expect(slot).toContainText('Not available yet');
		await page.keyboard.press('Home');
		for (let step = 0; step < 14 && !await slot.evaluate((node) => node === document.activeElement); step++) {
			await page.keyboard.press('ArrowDown');
		}
		await expect(slot).toBeFocused();
		for (const key of ['Enter', 'Space']) {
			await page.keyboard.press(key);
			await expect(slot).toBeFocused();
			await expect(menu(page)).toBeVisible();
			expect(await snapshot()).toEqual(before);
		}
		// aria-disabled items remain discoverable, unlike native disabled buttons.
		await slot.click({ force: true });
		await expect(menu(page)).toBeVisible();
		expect(await snapshot()).toEqual(before);
		await page.keyboard.press('Home');
		await page.keyboard.press('a');
		await expect(slot).not.toBeFocused();
		await page.keyboard.press('Escape');
	}
	expect(requests).toEqual([]);
});

test('modifier selection and list-scoped select all do not queue or change favorites', async ({ page }) => {
	await seed(page);
	await libraryRow(page, 'coral').locator('.nt-library-row').click({ modifiers: ['Control'] });
	await expect(page.getByTestId('selection-count')).toHaveText('1 selected');
	await libraryRow(page, 'ember').locator('.nt-library-row').click({ modifiers: ['Shift'] });
	await expect(page.getByTestId('selection-count')).toHaveText('3 selected');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await libraryRow(page, 'tide').locator('.nt-library-row').focus();
	await page.keyboard.press('Control+a');
	await expect(page.getByTestId('selection-count')).toHaveText('4 selected');
	await page.keyboard.press('Space');
	await expect(page.getByTestId('selection-count')).toHaveText('3 selected');
	await libraryRow(page, 'coral').getByTestId(TESTIDS.trackMore).click();
	await menu(page).getByRole('menuitem', { name: 'Play next', exact: true }).click();
	await expect.poll(() => queueIds(page)).toEqual(['coral', 'dawn', 'ember']);
	await page.getByTestId(TESTIDS.trackSelectionToolbar).getByRole('button', { name: 'Clear selection', exact: true }).click();
	await expect(page.getByTestId('selection-count')).toHaveText('0 selected');
});

test('queue batch selection has keyboard insertion parity and preserves the current track', async ({ page }) => {
	await seed(page);
	await queueAll(page);
	const current = await page.getByTestId('now-playing').textContent();
	await queueRow(page, 'dawn').getByRole('checkbox').check();
	await queueRow(page, 'tide').getByRole('checkbox').click({ modifiers: ['Shift'] });
	const toolbar = page.getByTestId(TESTIDS.queueSelectionToolbar);
	await expect(toolbar.getByRole('status')).toHaveText('3 selected in queue');
	await toolbar.getByLabel('Move selected before').selectOption('coral');
	await toolbar.getByRole('button', { name: 'Move selected', exact: true }).click();
	await expect.poll(() => queueIds(page)).toEqual(['dawn', 'ember', 'tide', 'coral']);
	await expect(page.getByTestId('now-playing')).toHaveText(current!);
	await expect(queueRow(page, 'coral')).toHaveAttribute('aria-current', 'true');
	await queueRow(page, 'dawn').getByTestId(TESTIDS.queueMore).click();
	await expect(menu(page)).toHaveAttribute('aria-label', '3 selected tracks');
	await menu(page).getByRole('menuitem', { name: 'Move to bottom', exact: true }).click();
	await expect.poll(() => queueIds(page)).toEqual(['coral', 'dawn', 'ember', 'tide']);
	await toolbar.getByRole('button', { name: 'Remove selected from queue' }).click();
	await expect.poll(() => queueIds(page)).toEqual(['coral']);
});

for (const action of ['Move selected', 'Play next']) {
	test(`queue toolbar ${action} retains queue order after reverse selection`, async ({ page }) => {
		await seed(page);
		await queueAll(page);
		await queueRow(page, 'ember').getByRole('checkbox').check();
		await queueRow(page, 'dawn').getByRole('checkbox').check();
		const toolbar = page.getByTestId(TESTIDS.queueSelectionToolbar);
		await toolbar.getByLabel('Move selected before').selectOption('tide');
		await toolbar.getByRole('button', { name: action, exact: true }).click();
		await expect.poll(() => queueIds(page)).toEqual(['coral', 'dawn', 'ember', 'tide']);
		await expect(queueRow(page, 'coral')).toHaveAttribute('aria-current', 'true');
	});
}

test('batch Play now keeps the old current track when it follows the first selection', async ({ page }) => {
	await page.addInitScript(() => {
		(window as unknown as { playCalls: number }).playCalls = 0;
		HTMLMediaElement.prototype.play = function () {
			(window as unknown as { playCalls: number }).playCalls++;
			return Promise.resolve();
		};
	});
	await page.route('**/interaction-audio/*', (route) => route.fulfill({ status: 204 }));
	await seed(page, ITEMS.map((item) => ({ ...item, streamUrl: `/interaction-audio/${item.id}` })));
	await queueAll(page);
	await queueRow(page, 'ember').getByRole('button', { name: /^Select Ember Sky/ }).click();
	await libraryRow(page, 'ember').getByRole('checkbox').check();
	await libraryRow(page, 'dawn').getByRole('checkbox').check();
	await libraryRow(page, 'dawn').getByTestId(TESTIDS.trackMore).click();
	await menu(page).getByRole('menuitem', { name: 'Play now', exact: true }).click();
	await expect.poll(() => queueIds(page)).toEqual(['coral', 'dawn', 'ember', 'tide']);
	await expect(queueRow(page, 'dawn')).toHaveAttribute('aria-current', 'true');
	await expect(page.getByTestId('queue-announcement')).toHaveText('Playing Dawn Chorus. Added 0, 2 already queued.');
	await expect.poll(() => page.evaluate(() => (window as unknown as { playCalls: number }).playCalls)).toBe(1);
});

test('narrow row retains its primary action alongside More', async ({ page }) => {
	await page.setViewportSize({ width: 320, height: 844 });
	await seed(page);
	await libraryRow(page, 'coral').click();
	await expect.poll(() => queueIds(page)).toEqual(['coral']);
	await expect(menu(page)).toHaveCount(0);
});

for (const path of ['/library/liked', '/library/recent', '/search?q=Coral', '/demo/player']) {
	test(`${path} does not opt into track menus or drag controls`, async ({ page }) => {
		await seed(page);
		await libraryRow(page, 'coral').getByRole('button', { name: /^Star Coral Night/ }).click();
		await page.goto(path);
		const main = page.getByRole('main');
		await expect(main).toBeVisible();
		await expect(main.locator('[draggable="true"], .queue-grip')).toHaveCount(0);
		await expect(page.getByTestId(TESTIDS.trackMore)).toHaveCount(0);
		await expect(page.getByTestId(TESTIDS.favoriteMore)).toHaveCount(0);
		await expect(page.getByTestId(TESTIDS.queueMore)).toHaveCount(0);
		await main.click({ button: 'right', position: { x: 10, y: 10 } });
		await expect(menu(page)).toHaveCount(0);
		await page.keyboard.press('Shift+F10');
		await expect(menu(page)).toHaveCount(0);
	});
}

test('native drag inserts into an empty queue, rejects external data and supports queue reordering', async ({ page }) => {
	await page.setViewportSize({ width: 1280, height: 1800 });
	await seed(page);
	await libraryRow(page, 'coral').dragTo(page.getByTestId(TESTIDS.queueDropTarget), { sourcePosition: { x: 4, y: 4 } });
	await expect.poll(() => queueIds(page)).toEqual(['coral']);
	await libraryRow(page, 'dawn').locator('.nt-library-row').click();
	await libraryRow(page, 'ember').locator('.nt-library-row').click();
	await queueRow(page, 'ember').getByRole('button', { name: /^Reorder / }).dragTo(queueRow(page, 'coral'), { targetPosition: { x: 4, y: 1 } });
	await expect.poll(() => queueIds(page)).toEqual(['ember', 'coral', 'dawn']);
	await expect(queueRow(page, 'coral')).toHaveAttribute('aria-current', 'true');
	await page.getByTestId(TESTIDS.queueDropTarget).evaluate((target) => {
		const dataTransfer = new DataTransfer();
		dataTransfer.setData('application/x-nntune-track-ids', 'forged');
		target.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer }));
	});
	await expect.poll(() => queueIds(page)).toEqual(['ember', 'coral', 'dawn']);
});

test('library batch Play now uses displayed order and hydrates only its first track', async ({ page }) => {
	await page.addInitScript(() => {
		(window as unknown as { playCalls: number }).playCalls = 0;
		HTMLMediaElement.prototype.play = function () {
			(window as unknown as { playCalls: number }).playCalls++;
			return Promise.resolve();
		};
	});
	await page.route('**/interaction-audio/*', (route) => route.fulfill({ status: 204 }));
	await seed(page, ITEMS.map((item) => ({ ...item, streamUrl: `/interaction-audio/${item.id}` })));
	await libraryRow(page, 'coral').locator('.nt-library-row').click();
	await libraryRow(page, 'ember').getByRole('checkbox').check();
	await libraryRow(page, 'dawn').getByRole('checkbox').check();
	await libraryRow(page, 'ember').getByTestId(TESTIDS.trackMore).click();
	await expect(menu(page)).toHaveAttribute('aria-label', '2 selected tracks');
	await menu(page).getByRole('menuitem', { name: 'Play now', exact: true }).click();
	await expect.poll(() => queueIds(page)).toEqual(['coral', 'dawn', 'ember']);
	await expect(queueRow(page, 'dawn')).toHaveAttribute('aria-current', 'true');
	await expect(page.getByTestId('queue-announcement')).toHaveText('Playing Dawn Chorus. Added 2.');
	await expect.poll(() => page.evaluate(() => (window as unknown as { playCalls: number }).playCalls)).toBe(1);
	await queueRow(page, 'ember').getByRole('button', { name: /^Select Ember Sky/ }).click();
	await expect(page.locator('audio')).toHaveCount(0);
});

test('mixed stars have one aggregate outcome and shelf menus retain local management behavior', async ({ page }) => {
	await seed(page);
	await libraryRow(page, 'coral').getByRole('button', { name: 'Star Coral Night', exact: true }).click();
	await libraryRow(page, 'coral').getByRole('checkbox').check();
	await libraryRow(page, 'dawn').getByRole('checkbox').check();
	await libraryRow(page, 'coral').getByTestId(TESTIDS.trackMore).click();
	await menu(page).getByRole('menuitem', { name: 'Star all', exact: true }).click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);
	await expect(page.getByTestId('favorites-announcement')).toHaveText('Starred 1 track; 1 was already starred.');
	const shelf = (id: string) => page.locator(`[data-testid="favorite-item"][data-id="${id}"]`);
	const more = shelf('coral').getByTestId(TESTIDS.favoriteMore);
	await expect(more).toHaveText('');
	await expect(more.locator('svg[aria-hidden="true"] circle')).toHaveCount(3);
	await more.press('Shift+F10');
	await expect(menu(page)).toHaveAttribute('aria-label', 'Actions for Coral Night');
	await expect(menu(page).getByRole('menuitem', { name: 'Move up', exact: true })).toHaveCount(0);
	await menu(page).getByRole('menuitem', { name: 'Move down', exact: true }).click();
	await expect(page.getByTestId('favorite-item').first()).toHaveAttribute('data-id', 'dawn');
	await expect(more).toBeFocused();
	await more.click();
	const unstar = menu(page).getByRole('menuitem', { name: 'Unstar', exact: true });
	await expect(unstar).not.toHaveAttribute('aria-checked');
	await expect(unstar).toHaveText('Unstar');
	await unstar.click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect(shelf('dawn').locator('[data-shelf-unstar]')).toBeFocused();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('queue Star and Unstar actions are verbs without checkbox state or checkmarks', async ({ page }) => {
	await seed(page);
	await queueAll(page);
	const more = queueRow(page, 'coral').getByTestId(TESTIDS.queueMore);
	for (const name of ['Star', 'Unstar']) {
		await more.click();
		const action = menu(page).getByRole('menuitem', { name, exact: true });
		await expect(action).not.toHaveAttribute('aria-checked');
		await expect(action).toHaveText(name);
		await action.click();
		await expect(menu(page)).toHaveCount(0);
		await expect(more).toBeFocused();
	}
});

test('queue grip opens at Move and menu removal focuses the next surviving More control', async ({ page }) => {
	await seed(page);
	await queueAll(page);
	const grip = queueRow(page, 'dawn').getByRole('button', { name: 'Reorder Dawn Chorus', exact: true });
	await grip.focus();
	await page.keyboard.press('Enter');
	await expect(menu(page).getByRole('menuitem', { name: 'Move to top', exact: true })).toBeFocused();
	await expect(menu(page).getByRole('menuitem', { name: 'Select track', exact: true })).toHaveCount(0);
	await expect(menu(page).getByRole('menuitem').last()).toHaveText('Remove from queue');
	await menu(page).getByRole('menuitem', { name: 'Remove from queue', exact: true }).click();
	await expect(queueRow(page, 'ember').getByTestId(TESTIDS.queueMore)).toBeFocused();
	await expect.poll(() => queueIds(page)).toEqual(['coral', 'ember', 'tide']);
});

for (const outcome of ['accepted', 'blocked', 'no-stream', 'superseded'] as const) {
	test(`queue Play now is an explicit, single playback attempt: ${outcome}`, async ({ page }) => {
		await page.addInitScript((outcome) => {
			const scope = window as unknown as { playCalls: number; finishPlay?: () => void };
			scope.playCalls = 0;
			HTMLMediaElement.prototype.play = function () {
				scope.playCalls += 1;
				if (outcome === 'blocked') return Promise.reject(new DOMException('Blocked', 'NotAllowedError'));
				if (outcome === 'superseded') return new Promise<void>((resolve) => { scope.finishPlay = resolve; });
				return Promise.resolve();
			};
		}, outcome);
		await page.route('**/interaction-audio/*', (route) => route.fulfill({ status: 204 }));
		await seed(page, ITEMS.map((item) => ({ ...item,
			...(item.id === 'dawn' && outcome !== 'no-stream' ? { streamUrl: '/interaction-audio/dawn' } : {})
		})));
		// Menu insertion is metadata-only, even when the library has a stream.
		await libraryRow(page, 'dawn').getByTestId(TESTIDS.trackMore).click();
		await menu(page).getByRole('menuitem', { name: 'Add to queue', exact: true }).click();
		await libraryRow(page, 'coral').locator('.nt-library-row').click();
		await expect(page.locator('audio')).toHaveCount(0);
		await queueRow(page, 'dawn').getByTestId(TESTIDS.queueMore).click();
		await menu(page).getByRole('menuitem', { name: 'Play now', exact: true }).click();
		await expect(queueRow(page, 'dawn')).toHaveAttribute('aria-current', 'true');
		await expect.poll(() => page.evaluate(() => (window as unknown as { playCalls: number }).playCalls))
			.toBe(outcome === 'no-stream' ? 0 : 1);
		if (outcome === 'superseded') {
			await queueRow(page, 'coral').getByRole('button', { name: /^Select Coral Night/ }).click();
			const announcement = await page.getByTestId('queue-announcement').textContent();
			await page.evaluate(() => (window as unknown as { finishPlay: () => void }).finishPlay());
			await expect(page.getByTestId('queue-announcement')).toHaveText(announcement!);
			await expect(queueRow(page, 'coral')).toHaveAttribute('aria-current', 'true');
		} else {
			await expect(page.getByTestId('queue-announcement')).toHaveText(outcome === 'accepted'
				? 'Playing Dawn Chorus.' : outcome === 'blocked'
					? 'Selected Dawn Chorus, paused: playback was blocked.' : 'Selected Dawn Chorus; no stream to play.');
		}
		await expect.poll(() => queueIds(page)).toEqual(['dawn', 'coral']);
	});
}

test('grouped drops resolve an after-row boundary beyond moving tracks', async ({ page }) => {
	await page.setViewportSize({ width: 1280, height: 1800 });
	await seed(page);
	await queueAll(page);
	await queueRow(page, 'coral').getByRole('checkbox').check();
	await queueRow(page, 'ember').getByRole('checkbox').check();
	const destination = queueRow(page, 'dawn');
	const box = (await destination.boundingBox())!;
	await queueRow(page, 'coral').getByRole('button', { name: /^Reorder / }).dragTo(destination, {
		targetPosition: { x: 4, y: box.height - 2 }
	});
	await expect.poll(() => queueIds(page)).toEqual(['dawn', 'coral', 'ember', 'tide']);
	await expect(queueRow(page, 'coral')).toHaveAttribute('aria-current', 'true');
});

test('Clear selection keeps library focus and queue Done recovers disappearing toolbar focus', async ({ page }) => {
	await seed(page);
	await libraryRow(page, 'coral').getByRole('checkbox').check();
	const clear = page.getByTestId(TESTIDS.trackSelectionToolbar).getByRole('button', { name: 'Clear selection', exact: true });
	await clear.focus();
	await page.keyboard.press('Enter');
	await expect(clear).toBeFocused();
	await libraryRow(page, 'coral').getByRole('checkbox').check();
	await page.getByLabel('Show', { exact: true }).selectOption('selected');
	await clear.focus();
	await page.keyboard.press('Enter');
	await expect(clear).toBeFocused();
	await page.getByLabel('Show', { exact: true }).selectOption('all');
	await queueAll(page);
	await queueRow(page, 'dawn').getByRole('checkbox').check();
	await page.getByTestId(TESTIDS.queueSelectionToolbar).getByRole('button', { name: 'Done', exact: true }).focus();
	await page.keyboard.press('Enter');
	await expect(queueRow(page, 'dawn').getByRole('checkbox')).toBeFocused();
});

for (const width of [1280, 390]) for (const colorScheme of ['light', 'dark'] as const) {
	test(`open menu and batch selection have no blocking axe violations at ${width}px in ${colorScheme}`, async ({ page }) => {
		await page.setViewportSize({ width, height: 1000 });
		await page.emulateMedia({ colorScheme });
		await seed(page);
		await queueAll(page);
		await queueRow(page, 'dawn').getByRole('checkbox').check();
		await queueRow(page, 'ember').getByRole('checkbox').check();
		await libraryRow(page, 'coral').getByTestId(TESTIDS.trackMore).click();
		await expect(menu(page)).toBeVisible();
		const results = await new AxeBuilder({ page }).analyze();
		expect(results.violations.filter((violation) =>
			violation.impact === 'serious' || violation.impact === 'critical')).toEqual([]);
		await page.keyboard.press('Escape');
		await queueRow(page, 'dawn').getByTestId(TESTIDS.queueMore).click();
		await expect(menu(page)).toHaveAttribute('aria-label', '2 selected tracks');
		const queueResults = await new AxeBuilder({ page }).analyze();
		expect(queueResults.violations.filter((violation) =>
			violation.impact === 'serious' || violation.impact === 'critical')).toEqual([]);
	});
}

for (const interruption of ['blur', 'scroll', 'hidden'] as const) {
	test(`pending touch long-press is canceled on ${interruption}`, async ({ browser }) => {
		const context = await browser.newContext({ viewport: { width: 390, height: 700 }, isMobile: true, hasTouch: true });
		const page = await context.newPage();
		await seed(page);
		const target = libraryRow(page, 'coral').locator('.nt-library-row');
		await target.scrollIntoViewIfNeeded();
		const source = (await target.boundingBox())!;
		await page.clock.install();
		const session = await context.newCDPSession(page);
		await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: source.x + 8, y: source.y + 8 }] });
		await page.evaluate((interruption) => {
			if (interruption === 'blur') window.dispatchEvent(new Event('blur'));
			else if (interruption === 'scroll') document.dispatchEvent(new Event('scroll'));
			else {
				Object.defineProperty(document, 'hidden', { configurable: true, value: true });
				document.dispatchEvent(new Event('visibilitychange'));
				Reflect.deleteProperty(document, 'hidden');
			}
		}, interruption);
		await page.clock.fastForward(600);
		await expect(menu(page)).toHaveCount(0);
		await session.send('Input.dispatchTouchEvent', { type: 'touchCancel', touchPoints: [] });
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		await context.close();
	});
}

test('hiding the page invalidates a native drag before a later drop', async ({ page }) => {
	await seed(page);
	await page.evaluate(() => {
		const source = document.querySelector('[data-testid="library-item"]')!;
		const destination = document.querySelector('[data-testid="queue-drop-target"]')!;
		const dataTransfer = new DataTransfer();
		source.dispatchEvent(new DragEvent('dragstart', { bubbles: true, cancelable: true, dataTransfer }));
		if (document.documentElement.dataset.trackDragging !== 'library') throw new Error('Drag did not start');
		Object.defineProperty(document, 'hidden', { configurable: true, value: true });
		document.dispatchEvent(new Event('visibilitychange'));
		Reflect.deleteProperty(document, 'hidden');
		destination.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer }));
	});
	await expect(page.locator('html')).not.toHaveAttribute('data-track-dragging');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('a refresh invalidates an open menu and its captured actions', async ({ page }) => {
	await seed(page);
	await libraryRow(page, 'coral').getByTestId(TESTIDS.trackMore).click();
	await expect(menu(page)).toBeVisible();
	const refresh = page.getByRole('button', { name: 'Refresh', exact: true });
	await refresh.click();
	await expect(menu(page)).toHaveCount(0);
	await expect(refresh).toBeFocused();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

for (const surface of ['library', 'favorites', 'queue'] as const) {
	test(`auth invalidation of the ${surface} menu recovers focus without stealing a newer choice`, async ({ page }) => {
		await seed(page);
		await queueAll(page);
		await libraryRow(page, 'coral').getByRole('button', { name: 'Star Coral Night', exact: true }).click();
		const more = surface === 'queue' ? queueRow(page, 'coral').getByTestId(TESTIDS.queueMore)
			: surface === 'favorites' ? page.getByTestId(TESTIDS.favoriteMore)
			: libraryRow(page, 'coral').getByTestId(TESTIDS.trackMore);
		await more.click();
		await expect(menu(page)).toBeVisible();
		await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', {
			key: 'nntune.library.auth-transition.v1', newValue: 'changed', storageArea: localStorage
		})));
		await expect(menu(page)).toHaveCount(0);
		await expect(page.getByTestId(TESTIDS.libraryItem)).toHaveCount(0);
		await expect(page.getByRole('button', { name: 'Retry', exact: true })).toBeFocused();
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(page.getByTestId(TESTIDS.libraryItem)).toHaveCount(ITEMS.length);
		await libraryRow(page, 'coral').getByTestId(TESTIDS.trackMore).click();
		await page.evaluate(() => {
			window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1' }));
			document.getElementById('library-density')?.focus();
		});
		await expect(page.getByLabel('Density', { exact: true })).toBeFocused();
	});
}

test('library toolbar reserves Play next before Append selected and includes hidden tracks in activation order', async ({ page }) => {
	await seed(page);
	const toolbar = page.getByTestId(TESTIDS.trackSelectionToolbar);
	const next = toolbar.getByRole('button', { name: 'Play next', exact: true });
	await expect(next).toBeDisabled();
	await expect(toolbar.getByRole('button', { name: 'Done', exact: true })).toHaveCount(0);
	await expect.poll(async () => (await toolbar.getByRole('button').allTextContents()).map((text) => text.trim()).slice(0, 4))
		.toEqual(['Select all', 'Clear selection', 'Play next', 'Append selected']);
	await libraryRow(page, 'coral').locator('.nt-library-row').click();
	await libraryRow(page, 'ember').getByRole('checkbox').check();
	await libraryRow(page, 'dawn').getByRole('checkbox').check();
	await page.getByLabel('Find in loaded tracks', { exact: true }).fill('Dawn');
	await expect(page.getByTestId('selection-hidden-count')).toContainText('1 selected track is hidden');
	await next.click();
	await expect.poll(() => queueIds(page)).toEqual(['coral', 'ember', 'dawn']);
	await expect(queueRow(page, 'coral')).toHaveAttribute('aria-current', 'true');
});

test('library touch More and grip targets meet the coarse-pointer minimum', async ({ browser }) => {
	const context = await browser.newContext({ viewport: { width: 390, height: 1000 }, isMobile: true, hasTouch: true });
	const page = await context.newPage();
	await seed(page);
	await queueAll(page);
	for (const control of [libraryRow(page, 'coral').getByTestId(TESTIDS.trackMore),
		queueRow(page, 'coral').getByTestId(TESTIDS.queueMore),
		queueRow(page, 'coral').getByRole('button', { name: 'Reorder Coral Night', exact: true })]) {
		const box = (await control.boundingBox())!;
		expect(box.width).toBeGreaterThanOrEqual(48);
		expect(box.height).toBeGreaterThanOrEqual(48);
	}
	await context.close();
});

test('touch grip reorders, Escape cancels, and non-grip touches leave the queue unchanged', async ({ browser }) => {
	const context = await browser.newContext({ viewport: { width: 390, height: 1000 }, isMobile: true, hasTouch: true });
	const page = await context.newPage();
	await seed(page);
	await queueAll(page);
	const source = queueRow(page, 'ember').getByRole('button', { name: /^Reorder / });
	await queueRow(page, 'dawn').scrollIntoViewIfNeeded();
	await source.scrollIntoViewIfNeeded();
	const start = (await source.boundingBox())!;
	const destination = (await queueRow(page, 'dawn').boundingBox())!;
	const session = await context.newCDPSession(page);
	const touch = async (type: 'touchStart' | 'touchMove' | 'touchEnd', x = 0, y = 0) => {
		await session.send('Input.dispatchTouchEvent', { type, touchPoints: type === 'touchEnd' ? [] : [{ x, y }] });
	};
	await touch('touchStart', start.x + 8, start.y + 8);
	await touch('touchMove', start.x + 18, start.y + 8);
	await expect(page.locator('html')).toHaveAttribute('data-track-dragging', 'queue');
	await touch('touchMove', destination.x + 4, destination.y + 3);
	await touch('touchEnd');
	await expect.poll(() => queueIds(page)).toEqual(['coral', 'ember', 'dawn', 'tide']);
	await expect(queueRow(page, 'coral')).toHaveAttribute('aria-current', 'true');
	const second = (await source.boundingBox())!;
	await touch('touchStart', second.x + 8, second.y + 8);
	await touch('touchMove', second.x + 18, second.y + 8);
	await expect(page.locator('html')).toHaveAttribute('data-track-dragging', 'queue');
	await page.keyboard.press('Escape');
	await touch('touchEnd');
	await expect(page.locator('html')).not.toHaveAttribute('data-track-dragging');
	await expect.poll(() => queueIds(page)).toEqual(['coral', 'ember', 'dawn', 'tide']);
	const body = (await queueRow(page, 'dawn').boundingBox())!;
	await touch('touchStart', body.x + 3, body.y + 20);
	await touch('touchMove', body.x + 3, body.y + 70);
	await touch('touchEnd');
	await expect(page.locator('html')).not.toHaveAttribute('data-track-dragging');
	await expect.poll(() => queueIds(page)).toEqual(['coral', 'ember', 'dawn', 'tide']);
	await context.close();
});

test('a stationary touch drag scrolls its queue pane and stops scrolling on drop', async ({ browser }) => {
	const context = await browser.newContext({ viewport: { width: 390, height: 900 }, isMobile: true, hasTouch: true });
	const page = await context.newPage();
	const items = [...ITEMS, ...Array.from({ length: 40 }, (_, i) => ({ id: `extra-${i}`, title: `Extra ${i}` }))];
	await seed(page, items);
	await page.getByRole('button', { name: 'Add displayed tracks to queue', exact: true }).click();
	const list = page.getByTestId(TESTIDS.queueDropTarget);
	await list.evaluate((node) => { node.style.maxHeight = '320px'; node.style.overflowY = 'auto'; });
	await list.scrollIntoViewIfNeeded();
	const source = (await queueRow(page, 'coral').getByRole('button', { name: /^Reorder / }).boundingBox())!;
	const box = (await list.boundingBox())!;
	const session = await context.newCDPSession(page);
	await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: source.x + 8, y: source.y + 8 }] });
	await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: source.x + 18, y: source.y + 8 }] });
	await expect(page.locator('html')).toHaveAttribute('data-track-dragging', 'queue');
	await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: box.x + 40, y: box.y + box.height - 8 }] });
	await expect.poll(() => list.evaluate((node) => node.scrollTop)).toBeGreaterThan(200);
	await page.keyboard.press('Escape');
	await session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
	await expect(page.locator('html')).not.toHaveAttribute('data-track-dragging');
	await expect.poll(() => queueIds(page)).toEqual(items.map((item) => item.id));
	expect(await list.evaluate(async (node) => {
		const before = node.scrollTop;
		await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
		return before === node.scrollTop;
	})).toBe(true);
	await list.evaluate((node) => { node.scrollTop = 0; });
	await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: source.x + 8, y: source.y + 8 }] });
	await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: source.x + 18, y: source.y + 8 }] });
	await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: box.x + 40, y: box.y + box.height - 8 }] });
	await expect.poll(() => list.evaluate((node) => node.scrollTop)).toBeGreaterThan(200);
	await session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
	await expect(page.locator('html')).not.toHaveAttribute('data-track-dragging');
	await expect(queueRow(page, 'coral')).toHaveAttribute('aria-current', 'true');
	await expect.poll(async () => (await queueIds(page)).indexOf('coral')).toBeGreaterThan(0);
	const stopped = await list.evaluate(async (node) => {
		const before = node.scrollTop;
		await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
		return before === node.scrollTop;
	});
	expect(stopped).toBe(true);
	await context.close();
});

test('menus stay in a narrow viewport and coarse-pointer long press opens a sheet', async ({ browser }) => {
	const context = await browser.newContext({ viewport: { width: 390, height: 700 }, isMobile: true, hasTouch: true });
	const page = await context.newPage();
	await seed(page);
	const target = libraryRow(page, 'coral').locator('.nt-library-row');
	await target.scrollIntoViewIfNeeded();
	const source = (await target.boundingBox())!;
	const session = await context.newCDPSession(page);
	await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: source.x + 8, y: source.y + 8 }] });
	await expect(menu(page)).toBeVisible();
	await session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	const box = await menu(page).boundingBox();
	expect(box!.x).toBeGreaterThanOrEqual(0);
	expect(box!.x + box!.width).toBeLessThanOrEqual(391);
	expect(box!.y + box!.height).toBeLessThanOrEqual(701);
	await expect(page.getByRole('menuitem', { name: 'Copy link', exact: true })).toBeVisible();
	await expect(page.getByRole('menuitem', { name: 'Add to playlist, not available yet' })).toHaveAttribute('aria-disabled', 'true');
	await page.keyboard.press('Escape');
	await expect(menu(page)).toHaveCount(0);
	await expect(libraryRow(page, 'coral').getByTestId(TESTIDS.trackMore)).toBeFocused();
	await context.close();
});

test('library Play now retains queued audio and plays once when refresh omits streamUrl', async ({ page }) => {
	await page.addInitScript(() => {
		(window as unknown as { playCalls: number }).playCalls = 0;
		HTMLMediaElement.prototype.play = function () {
			(window as unknown as { playCalls: number }).playCalls++;
			return Promise.resolve();
		};
	});
	await page.route('**/interaction-audio/*', (route) => route.fulfill({ status: 204 }));
	const withStream = ITEMS.map((item) => ({ ...item, streamUrl: `/interaction-audio/${item.id}` }));
	let current: typeof ITEMS = withStream;
	page.on('pageerror', (error) => pageErrors.push(error.message));
	await page.route('**/api/library', (route) => route.fulfill({ json: current }));
	await page.goto('/library');
	await expect(page.getByTestId(TESTIDS.libraryItem)).toHaveCount(ITEMS.length);
	await libraryRow(page, 'coral').locator('.nt-library-row').click();
	await expect.poll(() => queueIds(page)).toEqual(['coral']);
	await expect(page.locator('audio')).toHaveAttribute('src', /\/interaction-audio\/coral$/);
	const playsBefore = await page.evaluate(() => (window as unknown as { playCalls: number }).playCalls);
	current = ITEMS;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId(TESTIDS.libraryItem)).toHaveCount(ITEMS.length);
	await expect(queueRow(page, 'coral')).toBeVisible();
	await libraryRow(page, 'coral').getByTestId(TESTIDS.trackMore).click();
	await menu(page).getByRole('menuitem', { name: 'Play now', exact: true }).click();
	await expect(page.getByTestId('queue-announcement')).toContainText('Playing Coral Night');
	await expect.poll(() => queueIds(page)).toEqual(['coral']);
	await expect(page.locator('audio')).toHaveAttribute('src', /\/interaction-audio\/coral$/);
	await expect.poll(() => page.evaluate(() => (window as unknown as { playCalls: number }).playCalls)).toBe(playsBefore + 1);
	await expect(menu(page)).toHaveCount(0);
	expect(await page.evaluate(() => (window as unknown as { playCalls: number }).playCalls)).toBe(playsBefore + 1);
});

test('library Play next on only the current track is a silent no-op', async ({ page }) => {
	await seed(page);
	await libraryRow(page, 'coral').locator('.nt-library-row').click();
	await expect.poll(() => queueIds(page)).toEqual(['coral']);
	const announcement = page.getByTestId('queue-announcement');
	const before = await announcement.textContent();
	const nowPlaying = await page.getByTestId('now-playing').textContent();
	await libraryRow(page, 'coral').getByTestId(TESTIDS.trackMore).click();
	await menu(page).getByRole('menuitem', { name: 'Play next', exact: true }).click();
	await expect(menu(page)).toHaveCount(0);
	await expect.poll(() => queueIds(page)).toEqual(['coral']);
	await expect(queueRow(page, 'coral')).toHaveAttribute('aria-current', 'true');
	await expect(page.getByTestId('now-playing')).toHaveText(nowPlaying!);
	await expect(announcement).toHaveText(before ?? '');
	await expect(announcement).not.toContainText('Tracks placed next in the queue');
});

test('queue Move selected before resets to End of queue when the chosen target joins the batch', async ({ page }) => {
	await seed(page);
	await queueAll(page);
	const current = await page.getByTestId('now-playing').textContent();
	await queueRow(page, 'dawn').getByRole('checkbox').check();
	const toolbar = page.getByTestId(TESTIDS.queueSelectionToolbar);
	const select = toolbar.getByLabel('Move selected before');
	await select.selectOption('ember');
	await expect(select).toHaveValue('ember');
	await queueRow(page, 'ember').getByRole('checkbox').check();
	await expect(toolbar.getByRole('status')).toHaveText('2 selected in queue');
	await expect(select).toHaveValue('');
	await toolbar.getByRole('button', { name: 'Move selected', exact: true }).click();
	await expect.poll(() => queueIds(page)).toEqual(['coral', 'tide', 'dawn', 'ember']);
	await expect(queueRow(page, 'coral')).toHaveAttribute('aria-current', 'true');
	await expect(page.getByTestId('now-playing')).toHaveText(current!);
});

test('queue More is guarded with aria-disabled, not native disabled, during a deferred refresh', async ({ page }, testInfo) => {
	await seed(page);
	await queueAll(page);
	let release!: () => void;
	const gate = new Promise<void>((resolve) => { release = resolve; });
	await page.unroute('**/api/library');
	await page.route('**/api/library', async (route) => {
		await gate;
		await route.fulfill({ json: ITEMS });
	});
	const more = queueRow(page, 'coral').getByTestId(TESTIDS.queueMore);
	await more.focus();
	await expect(more).not.toHaveAttribute('aria-disabled', 'true');
	const before = await queueIds(page);
	const nowPlaying = await page.getByTestId('now-playing').textContent();
	try {
		await page.getByRole('button', { name: 'Refresh', exact: true }).evaluate((node: HTMLButtonElement) => node.click());
		await expect(more).toHaveAttribute('aria-disabled', 'true');
		expect(await more.evaluate((node: HTMLButtonElement) => node.disabled)).toBe(false);
		await expect(more).toBeFocused();
		await more.hover();
		await expect(more).toHaveCSS('cursor', 'default');
		await expect(more).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)');
		const screenshot = testInfo.outputPath('queue-more-unavailable.png');
		await page.screenshot({ path: screenshot, fullPage: true });
		await testInfo.attach('queue-more-unavailable', { path: screenshot, contentType: 'image/png' });
		await more.click({ force: true });
		await page.keyboard.press('Enter');
		await page.keyboard.press('Shift+F10');
		await expect(menu(page)).toHaveCount(0);
		await expect(more).toBeFocused();
		expect(await queueIds(page)).toEqual(before);
		await expect(page.getByTestId('now-playing')).toHaveText(nowPlaying!);
	} finally {
		release();
	}
	await expect(more).not.toHaveAttribute('aria-disabled', 'true');
	await more.click();
	await expect(menu(page)).toBeVisible();
	await page.keyboard.press('Escape');
	await expect(menu(page)).toHaveCount(0);
});

for (const gesture of ['Shift+Space', 'Shift+click'] as const) {
	test(`${gesture} on a primary that leaves Selected only recovers focus when the anchor is hidden by find`, async ({ page }) => {
		await seed(page);
		await libraryRow(page, 'coral').getByRole('checkbox').check();
		await libraryRow(page, 'dawn').getByRole('checkbox').check();
		const show = page.getByLabel('Show', { exact: true });
		await show.selectOption({ label: 'Selected only' });
		await page.getByLabel('Find in loaded tracks', { exact: true }).fill('Coral');
		await expect(page.getByTestId(TESTIDS.libraryItem)).toHaveCount(1);
		await expect(libraryRow(page, 'dawn')).toHaveCount(0);
		const primary = libraryRow(page, 'coral').locator('.nt-library-row');
		if (gesture === 'Shift+Space') {
			await primary.focus();
			await page.keyboard.press('Shift+Space');
		} else {
			await primary.click({ modifiers: ['Shift'] });
		}
		await expect(page.getByTestId(TESTIDS.libraryItem)).toHaveCount(0);
		await expect(page.locator('body')).not.toBeFocused();
		await expect(show).toBeFocused();
	});
}

test('queue Unstar under library Favorites only keeps focus on the queue More control', async ({ page }) => {
	await seed(page);
	await queueAll(page);
	await libraryRow(page, 'coral').getByRole('button', { name: 'Star Coral Night', exact: true }).click();
	const show = page.getByLabel('Show', { exact: true });
	await show.selectOption({ label: 'Favorites only' });
	await expect(libraryRow(page, 'coral')).toHaveCount(1);
	const more = queueRow(page, 'coral').getByTestId(TESTIDS.queueMore);
	await more.click();
	await menu(page).getByRole('menuitem', { name: 'Unstar', exact: true }).click();
	await expect(menu(page)).toHaveCount(0);
	await expect(libraryRow(page, 'coral')).toHaveCount(0);
	await expect(more).toBeFocused();
	await expect(show).not.toBeFocused();
});

test('a second touch on a different row cancels a pending long-press and keeps the opened-menu click guard', async ({ browser }) => {
	const context = await browser.newContext({ viewport: { width: 390, height: 900 }, isMobile: true, hasTouch: true });
	const page = await context.newPage();
	await seed(page);
	const first = libraryRow(page, 'coral').locator('.nt-library-row');
	const second = libraryRow(page, 'dawn').locator('.nt-library-row');
	await first.scrollIntoViewIfNeeded();
	const a = (await first.boundingBox())!;
	const b = (await second.boundingBox())!;
	await page.clock.install();
	const session = await context.newCDPSession(page);
	const one = { x: a.x + 8, y: a.y + 8, id: 1 };
	const two = { x: b.x + 8, y: b.y + 8, id: 2 };
	await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [one] });
	await page.clock.fastForward(200);
	await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [one, two] });
	await page.clock.fastForward(600);
	await expect(menu(page)).toHaveCount(0);
	await session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	// An already opened menu still suppresses its compatibility click after a secondary touch.
	await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [one] });
	await page.clock.fastForward(600);
	await expect(menu(page)).toBeVisible();
	await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [one, two] });
	await session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await context.close();
});
