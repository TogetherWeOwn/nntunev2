import { expect, test, type Page } from '@playwright/test';

/**
 * Lifecycle acceptance, independent of redeem failure classification.
 *
 * Removing the post-await mounted/operation guard must fail the abandonment
 * cases: a late success must not redirect the new page, replace its account
 * label, or invalidate its library. Late errors must not render old recovery
 * UI or a paywall over the new page. The fetch double deliberately ignores
 * AbortSignal: cancelling transport alone is not a post-await lifecycle guard.
 *
 * Removing branch-entry focus must fail the focus cases. Focus may land on a
 * heading or alert, not on body after the previously focused button disappears;
 * ordinary keyboard interaction must not keep resetting focus to that target.
 * All tokens, accounts and library rows are synthetic. No backend is contacted.
 */

const MAGIC_URL = '/auth/magic?token=fixture-lifecycle-token';
const LABEL_KEY = 'nntune:signed-in-email';
const CONTEXT_KEY = 'nntune.library.auth-context.v1';
const SNAPSHOT_KEY = 'nntune.library.snapshot.v1';
const CONTEXT = 'fixture-lifecycle-account';
const SNAPSHOT = JSON.stringify({
	savedAt: '2026-10-19T12:00:00.000Z',
	items: [{ id: 'fixture-lifecycle-track', title: 'Lifecycle Fixture Song' }]
});
const STORAGE_KEYS = [LABEL_KEY, CONTEXT_KEY, SNAPSHOT_KEY, `${SNAPSHOT_KEY}.${CONTEXT}`];

type Outcome = 200 | 401 | 429 | 503 | 'network-error';

interface PendingFetch {
	calls: number;
	release: () => void;
}

declare global {
	interface Window {
		__magicLifecycle?: PendingFetch;
		__magicDocument?: string;
	}
}

/** Hold only the external request; keep the API helper and all UI logic real. */
async function deferRequest(page: Page, endpoint: string, outcome: Outcome): Promise<void> {
	await page.addInitScript(
		({ path, result }) => {
			const originalFetch = window.fetch.bind(window);
			const pending: PendingFetch = { calls: 0, release: () => {} };
			window.__magicLifecycle = pending;
			window.fetch = (input, init) => {
				const url = new URL(input instanceof Request ? input.url : String(input), location.href);
				const method = init?.method ?? (input instanceof Request ? input.method : 'GET');
				if (url.pathname !== path || method.toUpperCase() !== 'POST') {
					return originalFetch(input, init);
				}
				pending.calls += 1;
				return new Promise<Response>((resolve, reject) => {
					pending.release = () => {
						if (result === 'network-error') {
							reject(new TypeError('fixture transport failure'));
							return;
						}
						resolve(
							new Response(
								JSON.stringify(
									result === 200
										? { ok: true, email: 'redeemed@example.test' }
										: result === 429
											? { code: 'rate_limited' }
											: { error: 'check your email and try again' }
								),
								{
									status: result,
									headers: {
										'Content-Type': 'application/json',
										...(result === 429 ? { 'Retry-After': '300' } : {})
									}
								}
							)
						);
					};
				});
			};
		},
		{ path: endpoint, result: outcome }
	);
}

async function flushRendering(page: Page): Promise<void> {
	// A controlled response, its JSON body and Svelte's microtask flush settle
	// before these frames. No wall-clock sleep races the held network request.
	await page.evaluate(
		() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())))
	);
}

async function releaseRequest(page: Page): Promise<void> {
	await page.evaluate(() => {
		if (!window.__magicLifecycle || window.__magicLifecycle.calls !== 1) {
			throw new Error('fixture request was not pending exactly once');
		}
		window.__magicLifecycle.release();
	});
	try {
		await flushRendering(page);
	} catch (error) {
		// A buggy late redirect (or the live success control) can replace the
		// document during the frame barrier. Let URL/storage assertions report
		// that behavior rather than an evaluate-context teardown error.
		if (!(error instanceof Error) || !error.message.includes('Execution context was destroyed')) {
			throw error;
		}
		await page.waitForLoadState();
	}
}

async function waitForRequest(page: Page): Promise<void> {
	await expect.poll(() => page.evaluate(() => window.__magicLifecycle?.calls)).toBe(1);
}

async function refuseRedeem(page: Page): Promise<void> {
	await page.route('**/v1/auth/magic/finish', (route) =>
		route.fulfill({
			status: 401,
			contentType: 'application/json',
			body: JSON.stringify({ error: 'check your email and try again' })
		})
	);
	await page.goto(MAGIC_URL);
	await page.getByRole('button', { name: /sign in with this link/i }).click();
	await expect(page.getByRole('alert').first()).toBeVisible();
}

async function seedCurrentAccount(page: Page): Promise<void> {
	await page.evaluate(
		({ labelKey, contextKey, snapshotKey, context, snapshot }) => {
			sessionStorage.setItem(labelKey, 'current-account@example.test');
			for (const storage of [sessionStorage, localStorage]) {
				storage.setItem(contextKey, context);
				storage.setItem(snapshotKey, snapshot);
				storage.setItem(`${snapshotKey}.${context}`, snapshot);
			}
		},
		{ labelKey: LABEL_KEY, contextKey: CONTEXT_KEY, snapshotKey: SNAPSHOT_KEY, context: CONTEXT, snapshot: SNAPSHOT }
	);
}

async function storedAccount(page: Page): Promise<(string | null)[][]> {
	return page.evaluate(
		(keys) => [sessionStorage, localStorage].map((storage) => keys.map((key) => storage.getItem(key))),
		STORAGE_KEYS
	);
}

/** Real SvelteKit link navigation, NOT page.goto (which destroys pending JS). */
async function leaveMagic(page: Page): Promise<void> {
	await page.evaluate(() => {
		window.__magicDocument = 'same-document-fixture';
		const leave = document.createElement('a');
		leave.href = '/pricing';
		leave.textContent = 'Leave pending magic link';
		leave.dataset.testid = 'leave-magic-fixture';
		document.body.append(leave);
	});
	await page.getByTestId('leave-magic-fixture').click();
	await expect(page).toHaveURL(/\/pricing$/);
	await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
	expect(await page.evaluate(() => window.__magicDocument)).toBe('same-document-fixture');
	await page.getByTestId('leave-magic-fixture').evaluate((element) => element.remove());
}

for (const outcome of [200, 401, 429, 'network-error'] as const) {
	test(`abandoned redeem (${outcome}) cannot affect the destination account or UI`, async ({ page }) => {
		await deferRequest(page, '/v1/auth/magic/finish', outcome);
		await page.goto(MAGIC_URL);
		await page.getByRole('button', { name: /sign in with this link/i }).click();
		await waitForRequest(page);
		await leaveMagic(page);
		// A new route/account owns this state before the old request settles.
		await seedCurrentAccount(page);
		const before = await storedAccount(page);
		const destination = await page.locator('main').innerText();

		await releaseRequest(page);

		expect.soft(await storedAccount(page), 'late redeem mutated destination storage').toEqual(before);
		await expect.soft(page).toHaveURL(/\/pricing$/);
		await expect.soft(page.locator('main')).toHaveText(destination, { useInnerText: true });
		await expect.soft(page.getByRole('dialog')).toHaveCount(0);
		await expect.soft(page.getByRole('alert')).toHaveCount(0);
	});
}

for (const outcome of [200, 401, 503, 'network-error'] as const) {
	test(`abandoned sign-out (${outcome}) cannot clear the destination account or publish old status`, async ({ page }) => {
		await deferRequest(page, '/v1/session/logout', outcome);
		await refuseRedeem(page);
		await page.getByRole('button', { name: /sign out/i }).click();
		await waitForRequest(page);
		await leaveMagic(page);
		await seedCurrentAccount(page);
		const before = await storedAccount(page);
		const destination = await page.locator('main').innerText();

		await releaseRequest(page);

		expect.soft(await storedAccount(page), 'late logout mutated destination storage').toEqual(before);
		await expect.soft(page).toHaveURL(/\/pricing$/);
		await expect.soft(page.locator('main')).toHaveText(destination, { useInnerText: true });
		await expect.soft(page.getByText(/could not sign out|signed out\. if that link/i)).toHaveCount(0);
		await expect.soft(page.getByRole('dialog')).toHaveCount(0);
	});
}

test('live redeem still clears old library state, labels the account and navigates', async ({ page }) => {
	await deferRequest(page, '/v1/auth/magic/finish', 200);
	await page.goto(MAGIC_URL);
	await seedCurrentAccount(page);
	await page.getByRole('button', { name: /sign in with this link/i }).click();
	await waitForRequest(page);
	await releaseRequest(page);

	await expect(page).toHaveURL(/\/onboarding\/byop$/);
	await expect(page.getByText(/signed in as/i)).toContainText('redeemed@example.test');
	expect(await page.evaluate((key) => sessionStorage.getItem(key), LABEL_KEY)).toBe('redeemed@example.test');
	for (const row of await storedAccount(page)) expect(row.slice(1)).toEqual([null, null, null]);
});

for (const outcome of [200, 401] as const) {
	test(`live sign-out (${outcome}) still clears the account and reaches retryable signed-out state`, async ({ page }) => {
		await deferRequest(page, '/v1/session/logout', outcome);
		await refuseRedeem(page);
		await seedCurrentAccount(page);
		await page.getByRole('button', { name: /sign out/i }).click();
		await waitForRequest(page);
		await releaseRequest(page);

		await expect(page.getByRole('status')).toContainText(/signed out/i);
		expect(await page.evaluate((key) => sessionStorage.getItem(key), LABEL_KEY)).toBeNull();
		for (const row of await storedAccount(page)) expect(row.slice(1)).toEqual([null, null, null]);
		await page.getByRole('button', { name: /try this link again/i }).click();
		await expect(page.getByRole('button', { name: /sign in with this link/i })).toBeEnabled();
	});
}

/** Allow either heading or alert, without dictating markup, tabindex or copy. */
async function expectBranchFocus(page: Page): Promise<void> {
	await expect
		.poll(() => page.evaluate(() => {
			const active = document.activeElement;
			return Boolean(active && document.querySelector('main')?.contains(active) &&
				active.matches('h1, h2, h3, [role="heading"], [role="alert"]'));
		}))
		.toBe(true);
	// Tab must leave the entry target. A working branch may have no controls,
	// in which case moving out to browser chrome/body is legitimate, not a trap.
	const entry = await page.locator(':focus').elementHandle();
	expect(entry).not.toBeNull();
	await page.keyboard.press('Tab');
	expect(await entry?.evaluate((element) => element === document.activeElement), 'entry focus must not trap Tab').toBe(false);
	const next = await page.evaluateHandle(() => document.activeElement);
	await flushRendering(page);
	expect(await next.evaluate((element) => element === document.activeElement), 'steady branch must not reset focus').toBe(true);
	await entry?.dispose();
	await next.dispose();
}

test('initial confirm branch puts focus on a stable entry target without trapping Tab', async ({ page }) => {
	await page.goto(MAGIC_URL);
	await expect(page.getByRole('button', { name: /sign in with this link/i })).toBeVisible();
	await expectBranchFocus(page);
});

test('missing-token error branch puts focus on its heading or alert', async ({ page }) => {
	await page.goto('/auth/magic');
	await expect(page.getByRole('alert').first()).toBeVisible();
	await expectBranchFocus(page);
});

test('confirm to working moves focus to a stable branch entry target', async ({ page }) => {
	await deferRequest(page, '/v1/auth/magic/finish', 200);
	await page.goto(MAGIC_URL);
	await page.getByRole('button', { name: /sign in with this link/i }).click();
	await waitForRequest(page);
	await expect(page.getByRole('status').filter({ hasText: /signing you in/i })).toBeVisible();
	await expectBranchFocus(page);
});

test('working to error focuses the new branch, not body', async ({ page }) => {
	await deferRequest(page, '/v1/auth/magic/finish', 401);
	await page.goto(MAGIC_URL);
	await page.getByRole('button', { name: /sign in with this link/i }).click();
	await waitForRequest(page);
	await releaseRequest(page);
	await expect(page.getByRole('alert').first()).toBeVisible();
	await expectBranchFocus(page);
});

test('error to signed-out focuses the new branch', async ({ page }) => {
	await deferRequest(page, '/v1/session/logout', 200);
	await refuseRedeem(page);
	await page.getByRole('button', { name: /sign out/i }).click();
	await waitForRequest(page);
	await releaseRequest(page);
	await expect(page.getByRole('status')).toContainText(/signed out/i);
	await expectBranchFocus(page);
});

test('signed-out to confirm focuses the returned branch', async ({ page }) => {
	await deferRequest(page, '/v1/session/logout', 200);
	await refuseRedeem(page);
	await page.getByRole('button', { name: /sign out/i }).click();
	await waitForRequest(page);
	await releaseRequest(page);
	await expect(page.getByRole('status')).toContainText(/signed out/i);
	await page.getByRole('button', { name: /try this link again/i }).click();
	await expect(page.getByRole('button', { name: /sign in with this link/i })).toBeVisible();
	await expectBranchFocus(page);
});

test('logout failure within the error branch does not steal focus from an interacting link', async ({ page }) => {
	await deferRequest(page, '/v1/session/logout', 503);
	await refuseRedeem(page);
	await page.getByRole('button', { name: /sign out/i }).click();
	await waitForRequest(page);
	const recoveryLink = page.getByRole('link', { name: /request a new one/i });
	await recoveryLink.focus();
	await releaseRequest(page);
	await expect(page.getByText(/could not sign out/i)).toBeVisible();
	await expect(recoveryLink).toBeFocused();
});

test('paywall return to confirm focuses the restored branch after dismissal', async ({ page }) => {
	await deferRequest(page, '/v1/auth/magic/finish', 429);
	await page.goto(MAGIC_URL);
	await page.getByRole('button', { name: /sign in with this link/i }).click();
	await waitForRequest(page);
	await releaseRequest(page);
	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await dialog.getByRole('button', { name: 'Not now' }).click();
	await expect(dialog).toBeHidden();
	await expect(page.getByRole('button', { name: /sign in with this link/i })).toBeVisible();
	await expectBranchFocus(page);
});
