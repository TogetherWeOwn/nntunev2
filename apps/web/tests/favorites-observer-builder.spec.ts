import { expect } from '@playwright/test';
import { test, expectMobileQueue, observedQueue, returnToMobileLibrary, metadataManagementState, installManagementMediaProbe, mobileQueueTransport } from './helpers/favorites-mobile';
import { openQueue } from './helpers/queue-host';

const items = [
	{ id: 'observer-alpha', title: 'Observer Alpha' },
	{ id: 'observer-bravo', title: 'Observer Bravo' },
	{ id: 'observer-charlie', title: 'Observer Charlie' }
];

test('selection version changes only with the actual owner selection', async ({ page, observeMobileLibrary }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await page.route('**/api/library', (route) => route.fulfill({ json: items }));
	await page.goto(await observeMobileLibrary());
	await page.getByRole('button', { name: 'Observer Alpha', exact: true }).click();
	const alpha = { ids: [items[0].id], currentIndex: 0, currentId: items[0].id, selectedTitle: items[0].title };
	const owner = await expectMobileQueue(page, alpha);
	const version = await owner.evaluate((binding) => binding.selectionVersion);
	expect(Number.isInteger(version)).toBe(true);
	if (version === undefined) throw new Error('Missing selection version');
	await returnToMobileLibrary(page);
	await page.getByRole('button', { name: 'Observer Bravo', exact: true }).click();
	const appended = { ...alpha, ids: [items[0].id, items[1].id] };
	await expectMobileQueue(page, appended);
	expect(await owner.evaluate((binding) => binding.selectionVersion)).toBe(version);
	await page.getByTestId('player-next').click();
	const bravo = { ...appended, currentIndex: 1, currentId: items[1].id, selectedTitle: items[1].title };
	await expect.poll(() => observedQueue(page)).toEqual(bravo);
	expect(await owner.evaluate((binding) => binding.selectionVersion)).toBe(version + 1);
	await returnToMobileLibrary(page);
	// Repeat publishes a new session state with the same queue selection.
	await page.getByTestId('player-repeat').selectOption('one');
	await expect(page.getByTestId('repeat-status')).toHaveText('Repeat: One');
	expect(await observedQueue(page)).toEqual(bravo);
	expect(await owner.evaluate((binding) => binding.selectionVersion)).toBe(version + 1);
	expect(await owner.evaluate((binding) => binding === window.__favoritesQueueOwner)).toBe(true);
});

for (const colorScheme of ['light', 'dark'] as const) {
	for (const instrumented of [false, true]) {
		test(`mobile queue ${instrumented ? 'observer calibration' : 'uninstrumented surface'} in ${colorScheme}`, async ({ page, observeMobileLibrary }) => {
			await page.setViewportSize({ width: 390, height: 844 });
			await page.emulateMedia({ colorScheme });
			const errors: string[] = [];
			page.on('pageerror', (error) => errors.push(error.message));
			await page.route('**/api/library', (route) => route.fulfill({ json: items }));
			await page.goto(instrumented ? await observeMobileLibrary() : '/library');
			await expect(page.getByTestId('library-item')).toHaveCount(3);
			for (const item of items) {
				await page.locator(`[data-testid="library-item"][data-id="${item.id}"]`).getByRole('button', { name: item.title, exact: true }).click();
			}
			await openQueue(page);
			await page.getByTestId('player-next').click();
			const expected = { ids: items.map((item) => item.id), currentIndex: 1, currentId: items[1].id, selectedTitle: items[1].title };
			const owner = instrumented ? await expectMobileQueue(page, expected) : null;
			if (!instrumented) expect(await page.evaluate(() => '__favoritesQueueOwner' in window)).toBe(false);
			const media = await page.locator('audio, video').evaluateAll((nodes) => nodes.map((node) => ({ src: (node as HTMLMediaElement).currentSrc, paused: (node as HTMLMediaElement).paused })));
			const check = async (state: typeof expected) => {
				await expect(page.getByTestId('queue-panel')).toHaveCount(1);
				await expect.poll(() => page.getByTestId('queue-item').evaluateAll((nodes) => nodes.map((node) => node.getAttribute('data-track-id')))).toEqual(state.ids);
				await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toHaveAttribute('data-track-id', state.currentId);
				await expect(page.getByTestId('now-playing')).toContainText(state.selectedTitle);
				if (owner) {
					expect(await owner.evaluate((binding) => binding === window.__favoritesQueueOwner)).toBe(true);
					expect(await observedQueue(page)).toEqual(state);
				}
				expect(await page.locator('audio, video').evaluateAll((nodes) => nodes.map((node) => ({ src: (node as HTMLMediaElement).currentSrc, paused: (node as HTMLMediaElement).paused })))).toEqual(media);
			};
			await check(expected);
			// Both changes go through the visible production queue, never the observer.
			await page.getByRole('button', { name: 'Select Observer Charlie', exact: true }).click();
			const changedCurrent = { ...expected, currentIndex: 2, currentId: items[2].id, selectedTitle: items[2].title };
			await check(changedCurrent);
			await page.getByRole('button', { name: 'Move Observer Charlie up', exact: true }).click();
			await check({ ...changedCurrent, ids: [items[0].id, items[2].id, items[1].id], currentIndex: 1 });
			await returnToMobileLibrary(page);
			await expect(page.getByTestId('queue-panel')).toHaveCount(0);
			await openQueue(page);
			await check({ ...changedCurrent, ids: [items[0].id, items[2].id, items[1].id], currentIndex: 1 });
			expect(errors).toEqual([]);
		});
	}
}

for (const colorScheme of ['light', 'dark'] as const) {
	test(`metadata evidence rejects broken observations and controls in ${colorScheme}`, async ({ page, observeMobileLibrary }) => {
		await page.setViewportSize({ width: 390, height: 844 });
		await page.emulateMedia({ colorScheme });
		await installManagementMediaProbe(page);
		await page.route('**/api/library', (route) => route.fulfill({ json: items }));
		await page.goto(await observeMobileLibrary());
		await expect(page.getByTestId('library-item')).toHaveCount(3);
		for (const item of items) await page.locator(`[data-testid="library-item"][data-id="${item.id}"]`).click();
		await openQueue(page);
		await page.getByTestId('player-next').click();
		const expected = { ids: items.map((item) => item.id), currentIndex: 1, currentId: items[1].id, selectedTitle: items[1].title };
		const owner = await expectMobileQueue(page, expected);
		const frozen = await observedQueue(page);
		await page.getByRole('button', { name: 'Select Observer Charlie', exact: true }).click();
		const changed = { ...expected, currentIndex: 2, currentId: items[2].id, selectedTitle: items[2].title };
		expect(await observedQueue(page)).toEqual(changed);
		// Fault only the test binding, never the session or its transitions.
		await page.evaluate((snapshot) => { window.__favoritesQueueOwner = { snapshot }; }, frozen);
		await expect(async () => expect(await observedQueue(page)).toEqual(changed)).rejects.toThrow();
		await page.evaluate(() => { delete window.__favoritesQueueOwner; });
		await expect(observedQueue(page)).rejects.toThrow(/Missing/);
		await owner.evaluate((binding) => { window.__favoritesQueueOwner = binding; });
		expect(await observedQueue(page)).toEqual(changed);

		const next = page.getByTestId('player-next');
		await next.evaluate((node: HTMLButtonElement) => { node.disabled = true; });
		await expect(mobileQueueTransport(page)).rejects.toThrow();
		await next.evaluate((node: HTMLButtonElement) => { node.disabled = false; node.removeAttribute('data-testid'); });
		await expect(mobileQueueTransport(page)).rejects.toThrow();
		await page.getByRole('button', { name: 'Next', exact: true }).evaluate((node) => { node.setAttribute('data-testid', 'player-next'); });
		await mobileQueueTransport(page);
		await returnToMobileLibrary(page);
		const baseline = await metadataManagementState(page);
		expect(baseline.calls).toEqual({ play: 0, pause: 0 });
		for (const kind of ['media', 'transport'] as const) {
			const injected = await page.evaluateHandle((kind) => {
				const node = document.createElement(kind === 'media' ? 'audio' : 'section');
				if (kind === 'transport') node.setAttribute('data-player-transport', '');
				document.body.append(node);
				return node;
			}, kind);
			await expect(metadataManagementState(page)).rejects.toThrow();
			await injected.evaluate((node) => node.remove());
			expect(await metadataManagementState(page)).toEqual(baseline);
		}
		await page.evaluate(() => { Reflect.get(window, '__managementMedia').play += 1; });
		await expect(async () => expect(await metadataManagementState(page)).toEqual(baseline)).rejects.toThrow();
		await page.evaluate(() => { Reflect.get(window, '__managementMedia').play -= 1; });
		expect(await metadataManagementState(page)).toEqual(baseline);
		await page.evaluate(() => { Reflect.deleteProperty(window, '__managementMedia'); });
		await expect(metadataManagementState(page)).rejects.toThrow(/Missing initialized/);
	});
}
