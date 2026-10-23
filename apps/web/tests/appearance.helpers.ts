import { expect, type Page, type TestInfo } from '@playwright/test';

export const APPEARANCE_KEY = 'nntune:appearance:v1';
export type Appearance = 'system' | 'light' | 'dark';
export type Palette = 'light' | 'dark';

// Hand-derived from the existing tokens, not a new palette. Comparing the full
// palette catches selectors that repaint only the body but leave controls mixed.
const palettes = {
	light: ['#f7f8fb', '#ffffff', '#141824', '#4d5670', '#2f4fe0', '#ffffff', '#dfe3ee', '#1a7f45', '#c0262d'],
	dark: ['#0b0d12', '#141824', '#f2f4f8', '#a7b0c2', '#7c9aff', '#0b0d12', '#2a3247', '#57d98d', '#ff7a7a']
};
const tokenNames = ['bg', 'bg-raised', 'fg', 'fg-muted', 'accent', 'accent-fg', 'border', 'good', 'bad'];

export function selector(page: Page) {
	return page.getByRole('combobox', { name: 'Appearance', exact: true });
}

export async function selected(page: Page, value: Appearance) {
	await expect(selector(page)).toHaveValue(value, { timeout: 2000 });
}

export async function choose(page: Page, value: Appearance) {
	await expect(selector(page)).toBeVisible({ timeout: 2000 });
	await selector(page).selectOption(value);
	await selected(page, value);
}

export async function assertPalette(page: Page, value: Palette) {
	await expect.poll(() => page.evaluate((names) => {
		const style = getComputedStyle(document.documentElement);
		return names.map((name) => style.getPropertyValue(`--nt-${name}`).trim().toLowerCase()
			.replace(/^#([a-f\d])([a-f\d])([a-f\d])$/, '#$1$1$2$2$3$3'));
	}, tokenNames), { timeout: 2000, message: `all existing tokens must resolve to the ${value} palette` }).toEqual(palettes[value]);
	await expect(page.locator('body')).toHaveCSS('background-color', value === 'light' ? 'rgb(247, 248, 251)' : 'rgb(11, 13, 18)');
	await expect(page.locator('body')).toHaveCSS('color', value === 'light' ? 'rgb(20, 24, 36)' : 'rgb(242, 244, 248)');
}

export async function assertNativeScheme(page: Page, value: Palette) {
	const mode = await selector(page).inputValue();
	// System may let the UA negotiate light/dark against the current OS. An
	// explicit override must constrain native controls to the chosen palette.
	for (const element of [page.locator('html'), selector(page)]) {
		await expect.poll(async () => {
			const scheme = await element.evaluate((node) => getComputedStyle(node).colorScheme
				.split(/\s+/).filter((word) => word !== 'only').join(' '));
			return mode === 'system' ? [value, 'light dark', 'dark light'].includes(scheme) : scheme === value;
		}, { timeout: 2000, message: `native controls must resolve consistently to ${value} in ${mode} mode` }).toBe(true);
	}
}

export async function openAppearance(page: Page, os: Palette, raw?: string) {
	await page.emulateMedia({ colorScheme: os });
	const errors = await installAudit(page);
	if (raw !== undefined) {
		await page.addInitScript(({ key, raw }) => localStorage.setItem(key, raw), { key: APPEARANCE_KEY, raw });
	}
	await page.goto('/login');
	await page.waitForLoadState('networkidle');
	return errors;
}

export async function stored(page: Page) {
	return page.evaluate((key) => localStorage.getItem(key), APPEARANCE_KEY);
}

export async function installAudit(page: Page) {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	page.on('console', (message) => {
		if (/hydrat|mismatch|content security policy|violates.*policy/i.test(message.text())) errors.push(message.text());
	});
	// Never call a provider/backend. API attempts are counted even though the
	// fixture contains them; an appearance write must not turn into an HTTP call.
	await page.route('**/*', async (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (/^\/(?:api|v1)(?:\/|$)/.test(url.pathname)) {
			return route.fulfill({ status: 503, contentType: 'application/json', body: '{}' });
		}
		return route.continue();
	});
	await page.addInitScript(() => {
		const audit = { csp: [] as string[], writes: [] as [string, string][], frames: [] as string[], documentId: crypto.randomUUID() };
		(window as Window & { __appearanceAudit?: typeof audit }).__appearanceAudit = audit;
		window.addEventListener('securitypolicyviolation', (event) => audit.csp.push(event.effectiveDirective));
		const setItem = Storage.prototype.setItem;
		Storage.prototype.setItem = function (key, value) {
			audit.writes.push([String(key), String(value)]);
			return setItem.call(this, key, value);
		};
		// Observe the first *renderable* application frame, not only the final DOM.
		// A hidden app may bootstrap, but must not reveal the wrong palette.
		function frame() {
			const main = document.querySelector('main');
			let visible = Boolean(main?.getClientRects().length);
			for (let element = main; element; element = element.parentElement) {
				const css = getComputedStyle(element);
				if (element.hidden || element.inert || css.visibility !== 'visible' || css.opacity === '0' || css.display === 'none') visible = false;
			}
			if (visible) audit.frames.push(getComputedStyle(document.documentElement).getPropertyValue('--nt-bg').trim().toLowerCase());
			if (audit.frames.length < 20) requestAnimationFrame(frame);
		}
		requestAnimationFrame(frame);
	});
	return errors;
}

export async function captureEvidence(page: Page, testInfo: TestInfo) {
	if (page.isClosed()) return;
	const state = await page.evaluate(() => ({
		path: location.pathname,
		os: matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light',
		background: getComputedStyle(document.body).backgroundColor,
		colorScheme: getComputedStyle(document.documentElement).colorScheme,
		selectors: [...document.querySelectorAll('select')].map((element) => ({ value: element.value, options: [...element.options].map((option) => option.text) })),
		mainVisible: Boolean(document.querySelector('main')?.getClientRects().length),
		csp: (window as Window & { __appearanceAudit?: { csp: string[] } }).__appearanceAudit?.csp ?? []
	}));
	await testInfo.attach('appearance-state', { body: JSON.stringify(state, null, 2), contentType: 'application/json' });
	await testInfo.attach('appearance-browser', { body: await page.screenshot(), contentType: 'image/png' });
}

export async function assertClean(page: Page, errors: string[]) {
	expect(errors, 'no hydration mismatch or uncaught errors').toEqual([]);
	expect(await page.evaluate(() => (window as Window & { __appearanceAudit?: { csp: string[] } }).__appearanceAudit?.csp), 'no CSP violations').toEqual([]);
}
