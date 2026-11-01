import { expect, test, type Page } from '@playwright/test';

/**
 * Regression: a held BYOP connection test must block overlapping entry.
 *
 * `runTest` in `src/routes/onboarding/byop/+page.svelte` acquires the
 * shared `busy` guard at handler entry. While a provider test response
 * is held open, the Test and Back controls must be disabled AND synthetic
 * entry (which bypasses `disabled`) must start no second test and cause
 * no step change; releasing the response settles the single test to ok
 * with Save visible.
 *
 * All API responses are synthetic fixtures with a controlled (held)
 * promise; no live provider.
 */

type JsonBody = Record<string, unknown>;

/** A promise released from test code, for holding a route open. */
function held(): { promise: Promise<void>; release: () => void } {
	let release!: () => void;
	const promise = new Promise<void>((resolve) => {
		release = resolve;
	});
	return { promise, release };
}

interface HeldTestWizard {
	testBodies: JsonBody[];
	releaseTest: () => void;
	firstTestStarted: Promise<void>;
}

/**
 * Route the provider test endpoint with the first response held open,
 * then drive the wizard to step 3 without testing yet.
 */
async function reachUntestedStep3(page: Page): Promise<HeldTestWizard> {
	const testBodies: JsonBody[] = [];
	const testHold = held();
	const firstTest = held();

	await page.route('**/v1/vault/test', async (route) => {
		testBodies.push(route.request().postDataJSON() as JsonBody);
		if (testBodies.length === 1) {
			firstTest.release();
			await testHold.promise;
		}
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true, latencyMs: 12 })
		});
	});

	await page.goto('/onboarding/byop');
	await page.getByLabel('Hostname').fill('news.example.test');
	await page.getByLabel('Username').fill('reader');
	await page.getByLabel('Password').fill('fixture-password');
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await expect(page.getByRole('heading', { name: 'Test the connection' })).toBeVisible();

	return {
		testBodies,
		releaseTest: testHold.release,
		firstTestStarted: firstTest.promise
	};
}

test('a held connection test blocks retest and back, then settles to ok', async ({ page }) => {
	const wizard = await reachUntestedStep3(page);

	await page.getByTestId('byop-test-connection').click();
	await wizard.firstTestStarted;
	await expect(page.getByTestId('byop-test-connection')).toHaveText(/Testing/);

	// Visible mutual exclusion: while the test response is held, no new
	// test can be started from the controls and Back is held too.
	await expect(page.getByTestId('byop-test-connection')).toBeDisabled();
	await expect(page.getByRole('button', { name: 'Back' })).toBeDisabled();

	// Handler-entry mutual exclusion: synthetic clicks (which bypass
	// `disabled`) must start no second test and cause no step change.
	await page.getByTestId('byop-test-connection').dispatchEvent('click');
	await page.getByRole('button', { name: 'Back' }).dispatchEvent('click');
	await expect(page.getByRole('heading', { name: 'Test the connection' })).toBeVisible();

	// Releasing the response settles the single test: Save appears for
	// the exact tested configuration and exactly one test was recorded.
	wizard.releaseTest();
	await expect(page.getByTestId('byop-save')).toBeVisible();
	await expect(page.getByTestId('byop-test-connection')).toBeEnabled();
	await expect(page.getByRole('button', { name: 'Back' })).toBeEnabled();
	expect(wizard.testBodies).toHaveLength(1);
});
