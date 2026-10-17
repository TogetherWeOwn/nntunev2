import { expect, test } from '@playwright/test';

/**
 * Branded 404 runtime contract (test-first): unknown paths must
 * serve the NNTune error page — branded 404 with a safe-navigation CTA to
 * `/library` and/or `/login` — with no stack trace, no default SvelteKit
 * chrome, and no secret/hostname/binding material in the rendered output.
 *
 * Fails until the Builder lands `src/routes/+error.svelte` on this branch
 * (default SvelteKit output has no NNTune brand or library/login CTA).
 * The Builder must not edit this file.
 */
const UNKNOWN_PATHS = ['/no-such-page-404-probe', '/definitely/not/a/route'];

const STACK_PATTERNS = [/stack\s*trace/i, /sveltekit/i, /__sveltekit/i, /at\s+\S+\s+\(.+:\d+:\d+\)/];

const LEAK_PATTERNS = [
	/secret/i,
	/api[_-]?key/i,
	/private[_-]?key/i,
	/bearer/i,
	/binding/i,
	/workers\.dev/i,
	/hostname/i,
	/\b(KV_|D1_|R2_)\w*/
];

test('unknown paths render a branded 404 with a safe-navigation CTA', async ({
	page,
	request
}) => {
	for (const path of UNKNOWN_PATHS) {
		const response = await request.get(path);
		expect(response.status(), `${path} should 404`).toBe(404);

		await page.goto(path);
		await expect(page.getByRole('heading', { level: 1 })).toContainText(/404/);
		await expect(page.locator('body')).toContainText(/NNTune/);

		const cta = page.locator('a[href="/library"], a[href="/login"]').first();
		await expect(cta, `safe-navigation CTA missing on ${path}`).toBeVisible();

		// The CTA must resolve to a real route, not another dead end.
		const href = await cta.getAttribute('href');
		const target = await request.get(href ?? '/does-not-resolve');
		expect(target.status(), `${href} CTA should not 404`).not.toBe(404);
	}
});

test('error page shows no stack trace or default SvelteKit chrome', async ({ page }) => {
	await page.goto(UNKNOWN_PATHS[0]!);

	const bodyText = (await page.locator('body').innerText()) ?? '';
	for (const pattern of STACK_PATTERNS) {
		expect(bodyText, `stack-trace/chrome pattern ${pattern} rendered`).not.toMatch(pattern);
	}

	// The default SvelteKit 404 ships its logo as the favicon; the branded
	// page inherits the NNTune favicon from the root layout instead. (The
	// `__sveltekit` bootstrap script ships on every SvelteKit page, branded
	// or not, so it must not be asserted on here.)
	const html = await page.content();
	expect(html, 'default SvelteKit favicon still served').not.toMatch(/svelte-logo/i);
});

test('error page leaks no secret, hostname, or binding material', async ({ page }) => {
	await page.goto(UNKNOWN_PATHS[1]!);

	const bodyText = (await page.locator('body').innerText()) ?? '';
	for (const pattern of LEAK_PATTERNS) {
		expect(bodyText, `leak pattern ${pattern} rendered`).not.toMatch(pattern);
	}

	const html = await page.content();
	expect(html).not.toMatch(/workers\.dev/i);
	expect(html).not.toMatch(/\$env\/(dynamic|static)\/private/);
});
