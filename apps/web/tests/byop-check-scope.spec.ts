import { expect, test, type Page } from '@playwright/test';

for (const withIndexer of [false, true]) {
	const flow = withIndexer ? 'with an indexer' : 'provider only';

	test.describe(flow, () => {
		test.beforeEach(async ({ page }) => {
			await page.goto('/onboarding/byop');
			await page.getByLabel('Hostname').fill('news.example.test');
			await page.getByLabel('Username').fill('reader');
			await page.getByLabel('Password', { exact: true }).fill('fixture-password');
			await page.getByRole('button', { name: 'Continue', exact: true }).click();
			if (withIndexer) {
				await page.getByLabel('Indexer URL').fill('https://indexer.example.test');
				await page.getByLabel('Indexer API key').fill('fixture-indexer-key');
			}
			await page.getByRole('button', { name: 'Continue', exact: true }).click();
		});

		test(`check scope and save gating ${flow}`, async ({ page }) => {
			let result: Record<string, unknown> = { ok: true };
			let releaseCheck: (() => void) | undefined;
			const checkPending = new Promise<void>((resolve) => { releaseCheck = resolve; });
			await page.route('**/v1/vault/test', async (route) => {
				await checkPending;
				await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(result) });
			});
			let saves = 0;
			await page.route('**/v1/vault', (route) => {
				saves += 1;
				return route.fulfill({ status: 200, contentType: 'application/json', body: '{"ok":true}' });
			});

			const section = page.getByRole('region', { name: 'Test the connection' });
			await expect(section).toContainText('validate the configuration and addresses');
			await expect(section).toContainText('does not verify provider credentials');
			if (withIndexer) {
				await expect(section).toContainText('including your indexer URL');
				await expect(section).toContainText('or contact or authenticate to your indexer');
			} else {
				await expect(section).not.toContainText('your indexer');
			}
			await expect(page.getByTestId('byop-save')).toHaveCount(0);
			await page.getByTestId('byop-test-connection').click();
			await expect(page.getByTestId('byop-test-connection')).toBeDisabled();
			await expect(page.getByTestId('byop-save')).toHaveCount(0);
			expect(saves).toBe(0);
			releaseCheck?.();

			const status = page.getByTestId('byop-status');
			await expect(status).toHaveAttribute('role', 'status');
			await expect(status).toContainText('Provider connectivity and configuration checks passed.');
			await expect(status).toContainText('No secrets were returned');
			await expect(page.getByTestId('byop-save')).toBeEnabled();
			await expect(section).not.toContainText('fixture-password');
			await expect(section).not.toContainText('fixture-indexer-key');

			// A later failed check must revoke even an earlier successful result.
			result = { ok: false };
			await page.getByTestId('byop-test-connection').click();
			await expect(status).toHaveAttribute('role', 'alert');
			await expect(status).toContainText('could not reach');
			await expect(page.getByTestId('byop-save')).toHaveCount(0);
			expect(saves).toBe(0);
		});

		test(`password retry describes checks, not password acceptance ${flow}`, async ({ page }) => {
			let attempts = 0;
			await page.route('**/v1/vault/test', (route) => {
				attempts += 1;
				return route.fulfill({
					status: attempts === 1 ? 401 : 200,
					contentType: 'application/json',
					body: JSON.stringify(attempts === 1
						? { code: 'provider_auth_expired', message: 'expired', retryable: false }
						: { ok: true, latencyMs: 12 })
				});
			});
			await page.getByTestId('byop-test-connection').click();
			const alert = page.getByTestId('byop-status');
			await expect(alert).toHaveAttribute('role', 'alert');
			await expect(alert).toContainText('connectivity and configuration checks before saving');
			await expect(alert).toContainText('These checks do not verify your password.');
			await expect(alert).not.toContainText('test the new password');
			await expect(alert).not.toContainText('new password is tested');
			await expect(page.getByLabel('Password', { exact: true })).toBeFocused();
			await page.getByLabel('Password', { exact: true }).fill('fixture-replacement-password');
			await expect(page.getByTestId('byop-save')).toHaveCount(0);
			await page.getByRole('button', { name: 'Test again', exact: true }).click();
			await expect(page.getByTestId('byop-status')).toContainText(
				'Provider connectivity and configuration checks passed in 12 ms.'
			);
			await expect(page.getByTestId('byop-save')).toBeEnabled();
			await expect(page.getByRole('region', { name: 'Test the connection' })).not.toContainText(
				'fixture-replacement-password'
			);
			expect(attempts).toBe(2);
		});
	});
}

const TRANSITION_RESTART_NOTICE =
	'This setup was reset after an authentication change. Sign in and start setup again; ' +
	're-enter and retest your credentials. Requests already submitted may still complete.';
const TRANSITION_KEY = 'nntune.library.auth-transition.v1';
const SIGNED_IN_EMAIL = 'nntune:signed-in-email';

interface TransitionHeld {
	release: (status: number, body: unknown) => Promise<void>;
	count: () => Promise<number>;
}

/**
 * Hold the connection-test boundary in-page so a late response still
 * arrives even when the page aborts its own request on reset: the fence,
 * not the transport, must suppress it.
 */
async function holdConnectionTest(page: Page): Promise<TransitionHeld> {
	await page.addInitScript(() => {
		const nativeFetch = window.fetch.bind(window);
		const hooks = window as unknown as {
			__heldTests: number;
			__settleTest?: (response: Response) => void;
		};
		hooks.__heldTests = 0;
		window.fetch = (input, init) => {
			const request = new Request(input, init);
			if (new URL(request.url).pathname !== '/v1/vault/test') {
				return nativeFetch(input, init);
			}
			return new Promise<Response>((resolve) => {
				hooks.__heldTests += 1;
				hooks.__settleTest = (response) => resolve(response);
			});
		};
	});
	return {
		release: (status, body) =>
			page.evaluate(
				({ status, body }) => {
					const settle = (
						window as unknown as { __settleTest?: (response: Response) => void }
					).__settleTest;
					if (!settle) throw new Error('No held connection test');
					settle(
						new Response(JSON.stringify(body), {
							status,
							headers: { 'content-type': 'application/json' }
						})
					);
				},
				{ status, body }
			),
		count: () =>
			page.evaluate(
				() => (window as unknown as { __heldTests: number }).__heldTests ?? -1
			)
	};
}

async function reachDraft(page: Page, tag: string): Promise<void> {
	await page.goto('/onboarding/byop');
	await page.getByLabel('Hostname').fill(`news.${tag}.example.test`);
	await page.getByLabel('Username').fill(`${tag}-reader`);
	await page.getByLabel('Password', { exact: true }).fill(`${tag}-probe-password`);
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await page.getByLabel('Indexer URL').fill(`https://indexer.${tag}.example.test`);
	await page.getByLabel('Indexer API key').fill(`${tag}-probe-indexer-key`);
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await expect(page.getByRole('region', { name: 'Test the connection' })).toBeVisible();
}

/** A second tab in the same context performs a real live logout. */
async function deliverTransition(page: Page): Promise<void> {
	const actor = await page.context().newPage();
	await actor.route('**/v1/vault', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({}) })
	);
	await actor.route('**/v1/session/logout', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true })
		})
	);
	await actor.goto('/account/security');
	await actor.getByRole('button', { name: /^sign out$/i }).click();
	await actor.waitForURL('**/login');
	await actor.close();
}

test.describe('delivered auth transition', () => {
	test('reset clears the draft and fences the late test response', async ({ page }) => {
		const pageErrors: string[] = [];
		page.on('pageerror', (error) => pageErrors.push(error.message));
		let saves = 0;
		let reauthPosts = 0;
		await page.route('**/v1/vault', async (route) => {
			saves += 1;
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({ ok: true })
			});
		});
		await page.route('**/v1/session/reauthenticate/**', async (route) => {
			reauthPosts += 1;
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({ ok: true })
			});
		});
		const held = await holdConnectionTest(page);
		await page.addInitScript((key) => {
			sessionStorage.setItem(key, 'transition-tab@example.com');
		}, SIGNED_IN_EMAIL);
		await reachDraft(page, 'delivered');
		await expect(page.getByText('Signed in as')).toBeVisible();
		await page.getByTestId('byop-test-connection').click();
		await expect.poll(() => held.count()).toBe(1);

		await deliverTransition(page);

		// Fail-closed restart: neutral notice with an explicit sign-in link,
		// identity/secrets/tested state cleared to step-1 defaults, no new
		// calls, no redirect.
		const notice = page.getByText(TRANSITION_RESTART_NOTICE);
		await expect(notice).toBeVisible();
		await expect(page.locator('a[href="/login"]')).toHaveCount(1);
		await expect(page.getByText('Signed in as')).toHaveCount(0);
		await expect(page.getByText('Step 1 of 3')).toBeVisible();
		await expect(page.getByLabel('Hostname')).toHaveValue('');
		await expect(page.getByLabel('Username')).toHaveValue('');
		await expect(page.getByLabel('Password', { exact: true })).toHaveValue('');
		expect(await page.evaluate((key) => sessionStorage.getItem(key), SIGNED_IN_EMAIL)).toBeNull();
		expect(await held.count()).toBe(1);
		expect(saves).toBe(0);
		expect(reauthPosts).toBe(0);
		expect.soft(new URL(page.url()).pathname).toBe('/onboarding/byop');

		// The held test response cannot restore tested state, status or
		// secrets, publish a paywall, or chain another request.
		await held.release(200, { ok: true, latencyMs: 5 });
		await page.waitForTimeout(350);
		await expect(notice).toBeVisible();
		await expect(page.getByTestId('byop-save')).toHaveCount(0);
		await expect(page.getByText(/checks passed/i)).toHaveCount(0);
		await expect(page.getByRole('dialog')).toHaveCount(0);
		expect(await held.count()).toBe(1);
		expect(saves).toBe(0);
		expect(reauthPosts).toBe(0);

		// Re-entry starts from empty secrets: the cleared indexer key is not
		// retained behind the fresh step either.
		await page.getByLabel('Hostname').fill('news.after.example.test');
		await page.getByLabel('Username').fill('after-reader');
		await page.getByLabel('Password', { exact: true }).fill('after-password-entry');
		await page.getByRole('button', { name: 'Continue', exact: true }).click();
		await expect(page.getByLabel('Indexer URL')).toHaveValue('');
		await expect(page.getByLabel('Indexer API key')).toHaveValue('');
		expect.soft(pageErrors).toEqual([]);
	});

	test('double delivery settles without loops, rebroadcasts or new calls', async ({
		page
	}) => {
		await page.addInitScript(
			({ email, transition }) => {
				sessionStorage.setItem(email, 'double-tab@example.com');
				const seen: string[] = [];
				(window as unknown as { __seenWrites: string[] }).__seenWrites = seen;
				for (const method of ['setItem', 'removeItem'] as const) {
					const original = Storage.prototype[method];
					Object.defineProperty(Storage.prototype, method, {
						configurable: true,
						value: function (this: Storage, ...args: string[]) {
							seen.push(`${method}:${args[0] ?? ''}`);
							return Reflect.apply(original, this, args);
						}
					});
				}
				void transition;
			},
			{ email: SIGNED_IN_EMAIL, transition: TRANSITION_KEY }
		);
		let tests = 0;
		let saves = 0;
		await page.route('**/v1/vault/test', async (route) => {
			tests += 1;
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({ ok: true, latencyMs: 5 })
			});
		});
		await page.route('**/v1/vault', async (route) => {
			saves += 1;
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({ ok: true })
			});
		});
		await reachDraft(page, 'double');
		await page.getByTestId('byop-test-connection').click();
		await expect(page.getByTestId('byop-save')).toBeEnabled();
		expect(tests).toBe(1);

		await deliverTransition(page);
		await deliverTransition(page);

		const notice = page.getByText(TRANSITION_RESTART_NOTICE);
		await expect(notice).toBeVisible();
		await expect(page.getByTestId('byop-save')).toHaveCount(0);
		expect(tests).toBe(1);
		expect(saves).toBe(0);
		const seen = await page.evaluate(
			() => (window as unknown as { __seenWrites: string[] }).__seenWrites
		);
		expect(
			seen.filter((entry) => entry === `setItem:${TRANSITION_KEY}`),
			'receivers never rebroadcast'
		).toEqual([]);
		expect.soft(new URL(page.url()).pathname).toBe('/onboarding/byop');
	});

	test('denied storage still resets the draft without throwing', async ({ page }) => {
		const pageErrors: string[] = [];
		page.on('pageerror', (error) => pageErrors.push(error.message));
		await page.addInitScript(() => {
			(window as unknown as { __rejections: string[] }).__rejections = [];
			window.addEventListener('unhandledrejection', (event) => {
				(window as unknown as { __rejections: string[] }).__rejections.push(String(event.reason));
			});
		});
		await page.route('**/v1/vault/test', (route) =>
			route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({ ok: true, latencyMs: 5 })
			})
		);
		await reachDraft(page, 'denied');
		await page.getByTestId('byop-test-connection').click();
		await expect(page.getByTestId('byop-save')).toBeEnabled();
		await page.evaluate(() => {
			for (const name of ['setItem', 'removeItem'] as const) {
				Object.defineProperty(Storage.prototype, name, {
					configurable: true,
					value: function () {
						throw new DOMException('fixture storage denied', 'SecurityError');
					}
				});
			}
		});

		await deliverTransition(page);

		// The in-memory reset does not depend on storage success.
		const notice = page.getByText(TRANSITION_RESTART_NOTICE);
		await expect(notice).toBeVisible();
		await expect(page.getByText('Step 1 of 3')).toBeVisible();
		await expect(page.getByLabel('Password', { exact: true })).toHaveValue('');
		await expect(page.getByTestId('byop-save')).toHaveCount(0);
		expect.soft(new URL(page.url()).pathname).toBe('/onboarding/byop');
		expect(
			await page.evaluate(
				() => (window as unknown as { __rejections: string[] }).__rejections
			),
			'no unhandled rejection'
		).toEqual([]);
		expect.soft(pageErrors, 'no page error').toEqual([]);
	});
});
