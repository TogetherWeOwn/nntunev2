import { writeFile } from 'node:fs/promises';
import { expect, test } from '@playwright/test';
import { TESTIDS } from '../../../e2e/helpers/contract';

// Retain the original Copy-link gestures and focus assertions while observing
// the outside-click event order. The production menu and existing bank are unchanged.
test('Copy-link outside dismissal retains focus-event evidence', async ({ page }, testInfo) => {
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	await page.addInitScript(() => {
		Object.defineProperty(navigator, 'clipboard', { value: { writeText: async (url: string) => {
			(window as unknown as { copiedLink: string }).copiedLink = url;
		} } });
		const records: unknown[] = [];
		const describe = (node: EventTarget | null) => node instanceof Element ? {
			tag: node.tagName, testid: node.getAttribute('data-testid'), role: node.getAttribute('role'),
			id: node.id, label: node.getAttribute('aria-label'), connected: node.isConnected,
			text: node.textContent?.slice(0, 100)
		} : null;
		const sample = (type: string, target: EventTarget | null, related: EventTarget | null = null) => {
			records.push({ type, time: performance.now(), target: describe(target), related: describe(related),
				active: describe(document.activeElement), menus: document.querySelectorAll('[data-testid="track-context-menu"]').length,
				scrollX: window.scrollX, scrollY: window.scrollY });
		};
		for (const type of ['pointerdown', 'mousedown', 'focusin', 'focusout', 'click', 'scroll']) {
			document.addEventListener(type, (event) => {
				sample(type, event.target, event instanceof FocusEvent ? event.relatedTarget : null);
				queueMicrotask(() => sample(`${type}:microtask`, event.target));
			}, true);
		}
		(window as unknown as { menuFocusEvidence: { records: unknown[]; mark: (name: string) => void } }).menuFocusEvidence = {
			records, mark: (name) => sample(name, document.activeElement)
		};
	});
	try {
		await page.route('**/api/library', (route) => route.fulfill({ json: [
			{ id: 'coral', title: 'Coral Night', artist: 'Harbor Lights' },
			{ id: 'dawn', title: 'Dawn Chorus' },
			{ id: 'ember', title: 'Ember Sky' },
			{ id: 'tide', title: 'Tide Pools' }
		] }));
		await page.goto('/library');
		await expect(page.getByTestId(TESTIDS.libraryItem)).toHaveCount(4);
		const more = page.locator('[data-testid="library-item"][data-id="coral"]').getByTestId(TESTIDS.trackMore);
		const menu = page.getByTestId(TESTIDS.trackContextMenu);
		await more.click();
		await expect(page.getByRole('menuitem', { name: 'Share', exact: true })).toHaveCount(0);
		await page.keyboard.press('End');
		await expect(page.getByRole('menuitem', { name: 'Copy link', exact: true })).toBeFocused();
		await page.keyboard.press('Enter');
		await expect(menu).toHaveCount(0);
		await expect(more).toBeFocused();
		await expect.poll(() => page.evaluate(() => (window as unknown as { copiedLink: string }).copiedLink))
			.toContain('/library?track=coral');
		await more.click();
		await page.keyboard.press('Tab');
		await expect(menu).toHaveCount(0);
		await expect(page.locator('body')).not.toBeFocused();
		await more.click();
		await page.evaluate(() => (window as unknown as { menuFocusEvidence: { mark: (name: string) => void } }).menuFocusEvidence.mark('before-heading-click'));
		await page.getByRole('heading', { name: 'Library', exact: true }).click();
		await expect(menu).toHaveCount(0);
		await expect(more).toBeFocused();
		await expect(pageErrors).toEqual([]);
	} finally {
		const records = await page.evaluate(() => (window as unknown as { menuFocusEvidence?: { records: unknown[] } }).menuFocusEvidence?.records ?? []);
		const path = testInfo.outputPath('menu-focus-events.json');
		await writeFile(path, JSON.stringify({ title: testInfo.title, status: testInfo.status, errors: testInfo.errors, pageErrors, records }, null, 2));
		await testInfo.attach('menu-focus-events', { path, contentType: 'application/json' });
	}
});
