import { expect, test } from '@playwright/test';

for (const withIndexer of [false, true]) {
	const flow = withIndexer ? 'with an indexer' : 'provider only';

	test.describe(flow, () => {
		test.beforeEach(async ({ page }) => {
			await page.goto('/onboarding/byop');
			await page.getByLabel('Hostname').fill('news.example.test');
			await page.getByLabel('Username').fill('reader');
			await page.getByLabel('Password', { exact: true }).fill('fixture-password');
			await page.getByRole('button', { name: 'Continue' }).click();
			if (withIndexer) {
				await page.getByLabel('Indexer URL').fill('https://indexer.example.test');
				await page.getByLabel('Indexer API key').fill('fixture-indexer-key');
			}
			await page.getByRole('button', { name: 'Continue' }).click();
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
