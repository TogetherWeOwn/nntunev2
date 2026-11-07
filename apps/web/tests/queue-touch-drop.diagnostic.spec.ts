import { writeFile } from 'node:fs/promises';
import { expect, test } from '@playwright/test';
import { TESTIDS } from '../../../e2e/helpers/contract';
import { openQueue } from './helpers/queue-host.js';

// Repeat the stationary-touch scenario without changing its original bank.
// Capture hit-testing before the grip's release handler changes drag state.
test('stationary queue touch release retains hit-test and cancellation evidence', async ({ browser }, testInfo) => {
	const context = await browser.newContext({ viewport: { width: 390, height: 900 }, isMobile: true, hasTouch: true });
	const page = await context.newPage();
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	await page.addInitScript(() => {
		const records: unknown[] = [];
		Object.assign(window, { queueTouchDiagnostic: records });
		const describe = (node: Element | null) => node ? {
			tag: node.tagName,
			testid: node.getAttribute('data-testid'),
			role: node.getAttribute('role'),
			trackId: node.closest('[data-track-id]')?.getAttribute('data-track-id'),
			connected: node.isConnected
		} : null;
		for (const type of ['pointerdown', 'pointermove', 'pointerup', 'pointercancel', 'lostpointercapture']) {
			document.addEventListener(type, (event) => {
				if (!(event instanceof PointerEvent) || event.pointerType === 'mouse') return;
				const list = document.querySelector('[data-testid="queue-drop-target"]');
				const hit = document.elementFromPoint(event.clientX, event.clientY);
				records.push({
					type, time: performance.now(), pointerId: event.pointerId,
					pointerType: event.pointerType, x: event.clientX, y: event.clientY,
					target: describe(event.target instanceof Element ? event.target : null),
					hit: describe(hit), hitAncestors: hit ? [...function* () {
						for (let node: Element | null = hit; node; node = node.parentElement) yield describe(node);
					}()] : [],
					insideList: !!hit && !!list?.contains(hit), listBounds: list?.getBoundingClientRect().toJSON(),
					scrollTop: list?.scrollTop, dragging: document.documentElement.dataset.trackDragging,
					captured: event.target instanceof Element && event.target.hasPointerCapture(event.pointerId)
				});
			}, true);
		}
	});
	const items = [
		{ id: 'coral', title: 'Coral Night', artist: 'Harbor Lights' },
		{ id: 'dawn', title: 'Dawn Chorus' },
		{ id: 'ember', title: 'Ember Sky' },
		{ id: 'tide', title: 'Tide Pools' },
		...Array.from({ length: 40 }, (_, i) => ({ id: `extra-${i}`, title: `Extra ${i}` }))
	];
	const queueRow = (id: string) => page.getByTestId('queue-panel').locator(`[data-testid="queue-item"][data-track-id="${id}"]`);
	const queueIds = () => page.getByTestId('queue-panel').getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
	const stages: unknown[] = [];
	const snapshot = async (stage: string) => {
		stages.push(await page.evaluate((stage) => ({
			stage, time: performance.now(), dragging: document.documentElement.dataset.trackDragging,
			queue: [...document.querySelectorAll('[data-testid="queue-panel"] [data-testid="queue-item"]')]
				.map((row) => ({ id: row.getAttribute('data-track-id'), current: row.getAttribute('aria-current') })),
			scrollTop: document.querySelector('[data-testid="queue-drop-target"]')?.scrollTop
		}), stage));
	};
	try {
		await page.route('**/api/library', (route) => route.fulfill({ json: items }));
		await page.goto('/library');
		await expect(page.getByTestId(TESTIDS.libraryItem)).toHaveCount(items.length);
		await page.getByRole('button', { name: 'Add displayed tracks to queue', exact: true }).click();
		await openQueue(page);
		const list = page.getByTestId(TESTIDS.queueDropTarget);
		await list.evaluate((node) => { node.style.maxHeight = '320px'; node.style.overflowY = 'auto'; });
		// Match the real bank's visible drop-point prerequisite, not an obscured edge.
		await list.evaluate((node) => node.scrollIntoView({ block: 'center', behavior: 'instant' }));
		const source = (await queueRow('coral').getByRole('button', { name: /^Reorder / }).boundingBox())!;
		const box = (await list.boundingBox())!;
		const dropHitsList = () => list.evaluate((node, point) => node.contains(document.elementFromPoint(point.x, point.y)),
			{ x: box.x + 40, y: box.y + box.height - 8 });
		expect(await dropHitsList()).toBe(true);
		stages.push({ stage: 'gesture-coordinates', source, box });
		const session = await context.newCDPSession(page);
		await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: source.x + 8, y: source.y + 8 }] });
		await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: source.x + 18, y: source.y + 8 }] });
		await expect(page.locator('html')).toHaveAttribute('data-track-dragging', 'queue');
		await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: box.x + 40, y: box.y + box.height - 8 }] });
		await expect.poll(() => list.evaluate((node) => node.scrollTop)).toBeGreaterThan(200);
		await snapshot('before-escape');
		await page.keyboard.press('Escape');
		await session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
		await expect(page.locator('html')).not.toHaveAttribute('data-track-dragging');
		await expect.poll(queueIds).toEqual(items.map((item) => item.id));
		expect(await list.evaluate(async (node) => {
			const before = node.scrollTop;
			await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
			return before === node.scrollTop;
		})).toBe(true);
		await list.evaluate((node) => { node.scrollTop = 0; });
		await snapshot('before-second-drag');
		await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: source.x + 8, y: source.y + 8 }] });
		await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: source.x + 18, y: source.y + 8 }] });
		await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: box.x + 40, y: box.y + box.height - 8 }] });
		await expect.poll(() => list.evaluate((node) => node.scrollTop)).toBeGreaterThan(200);
		await snapshot('before-drop');
		expect(await dropHitsList()).toBe(true);
		await session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
		await expect(page.locator('html')).not.toHaveAttribute('data-track-dragging');
		await expect(queueRow('coral')).toHaveAttribute('aria-current', 'true');
		await expect.poll(async () => (await queueIds()).indexOf('coral')).toBeGreaterThan(0);
		const stopped = await list.evaluate(async (node) => {
			const before = node.scrollTop;
			await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
			return before === node.scrollTop;
		});
		expect(stopped).toBe(true);
		await expect(pageErrors).toEqual([]);
	} finally {
		let records: unknown = null;
		let collectionError: string | null = null;
		try {
			await snapshot('final');
			records = await page.evaluate(() => (window as unknown as { queueTouchDiagnostic?: unknown }).queueTouchDiagnostic);
		} catch (error) { collectionError = String(error); }
		const path = testInfo.outputPath('queue-touch-release.json');
		await writeFile(path, JSON.stringify({ stages, records, pageErrors, collectionError }, null, 2));
		await testInfo.attach('queue-touch-release', { path, contentType: 'application/json' });
		await context.close();
	}
});
