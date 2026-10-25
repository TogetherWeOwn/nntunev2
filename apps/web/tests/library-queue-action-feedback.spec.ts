import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned remove/clear regressions supplementing the protected bank.
 * Run from apps/web with the production-build Playwright config:
 *   pnpm exec playwright test tests/library-queue-action-feedback.spec.ts --workers=1
 * All library responses are offline fixtures; no provider or audio requests.
 */
const ITEMS = [
	{ id: 'intro-one', title: 'Intro', artist: 'First Band' },
	{ id: 'intro-two', title: 'Intro', artist: 'Second Band' },
	{ id: 'outro', title: 'Outro' }
];

async function seedQueue(page: Page): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(ITEMS)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	for (const item of ITEMS) {
		await page.locator(`[data-testid="library-item"][data-id="${item.id}"]`).click();
	}
	await expect(page.getByTestId('queue-item')).toHaveCount(ITEMS.length);
}

function removeButton(page: Page, id: string) {
	return page
		.locator(`[data-testid="queue-item"][data-track-id="${id}"]`)
		.getByRole('button', { name: 'Remove Intro from queue', exact: true });
}

test('QF-R01 equal-title keyboard removals each update the same polite region once', async ({ page }) => {
	await seedQueue(page);
	const region = page.getByTestId('queue-announcement');
	await expect(region).toHaveAttribute('role', 'status');
	await region.evaluate((element) => {
		const commits: string[] = [];
		const observer = new MutationObserver(() => {
			const text = element.textContent ?? '';
			if (text !== '') commits.push(text);
		});
		observer.observe(element, { childList: true, characterData: true, subtree: true });
		Object.assign(window, { __queueFeedback: { region: element, commits, observer } });
	});
	const commits = () => page.evaluate(() =>
		(window as Window & { __queueFeedback: { commits: string[] } }).__queueFeedback.commits
	);

	await removeButton(page, 'intro-one').focus();
	await page.keyboard.press('Enter');
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expect(removeButton(page, 'intro-two')).toBeFocused();
	await expect.poll(commits).toEqual(['Intro removed from the queue.']);

	// Same text, different stable id: a new action must still update the region.
	await page.keyboard.press('Enter');
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(region).toHaveText('Intro removed from the queue.');
	await expect.poll(commits).toEqual([
		'Intro removed from the queue.',
		'Intro removed from the queue.'
	]);
	await expect(page.getByTestId('queue-announcement')).toHaveCount(1);
	expect(await region.evaluate((element) =>
		element === (window as Window & { __queueFeedback: { region: Element } }).__queueFeedback.region
	)).toBe(true);
	await expect(page.getByTestId('now-playing')).toContainText('Outro');
	await expect(page.locator('audio')).toHaveCount(0);
});

for (const action of ['remove', 'clear'] as const) {
	test(`QF-R0${action === 'remove' ? '2' : '3'} newer focus after ${action} dispatch wins over recovery`, async ({ page }) => {
		await seedQueue(page);
		const trigger = action === 'remove'
			? removeButton(page, 'intro-one')
			: page.getByRole('button', { name: 'Clear queue', exact: true });
		await trigger.focus();
		await expect(trigger).toBeFocused();

		// One synchronous task: the trigger still holds focus when the real
		// Svelte handler starts; a later bubbling listener supersedes it before
		// tick() resumes. Pre-dispatch Tab/dispatchEvent would miss this guard.
		const dispatch = await trigger.evaluate((element) => {
			const button = element as HTMLButtonElement;
			const sentinel = document.createElement('button');
			sentinel.type = 'button';
			sentinel.dataset.testid = 'queue-feedback-sentinel';
			sentinel.textContent = 'Later focus placement';
			document.querySelector('main')!.append(sentinel);
			let focusedAtDispatch = false;
			button.addEventListener('click', () => {
				focusedAtDispatch = document.activeElement === button;
			}, { once: true });
			document.addEventListener('click', () => sentinel.focus(), { once: true });
			button.click();
			return { focusedAtDispatch, newerFocusPlaced: document.activeElement === sentinel };
		});
		expect(dispatch).toEqual({ focusedAtDispatch: true, newerFocusPlaced: true });
		await expect(page.getByTestId('queue-item')).toHaveCount(action === 'remove' ? 2 : 0);
		await expect(page.getByTestId('queue-announcement')).toHaveText(
			action === 'remove' ? 'Intro removed from the queue.' : 'Queue cleared.'
		);
		await expect(page.getByTestId('queue-feedback-sentinel')).toBeFocused();
		await expect(page.locator('audio')).toHaveCount(0);
	});
}
