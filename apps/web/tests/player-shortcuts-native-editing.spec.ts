import { expect, test, type Page } from '@playwright/test';

/**
 * Protected KEYSAFE acceptance: real demo route, real DOM and key handler;
 * media decoder/network alone are doubled. No new production API/path pins.
 * Synthetic composition events establish guard behavior, NOT real OS IME support.
 *
 * Breaks caught: SELECT/OPTION omitted; an editable child intercepted; ignored
 * cancellation/composition; unsafe j/l seeks; overbroad guards breaking ordinary
 * buttons/audio/body or the help dialog. Existing shortcut/comfort/jump specs
 * remain protected and unchanged. Builder ships these tests with implementation,
 * never this intentionally RED test-only branch by itself.
 */
const KEYS = ['k', 'n', 'p', 'j', 'l', 'm', '?'];
type Probe = { calls: string[]; seeks: number[]; muteWrites: boolean[]; time: number; muted: boolean };
declare global {
	interface Window { __keySafety: Probe; }
}
type MediaSeed = {
	time?: number; duration?: number; readyState?: number;
	throws?: 'duration' | 'get-time' | 'set-time';
};

async function openPlayer(page: Page) {
	// Every request is local, with pending synthetic audio: no live providers.
	await page.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)) return route.abort();
		if (url.pathname === '/api/demo/audio') return;
		if (url.pathname.startsWith('/api/')) return route.abort();
		return route.continue();
	});
	await page.addInitScript(() => {
		sessionStorage.clear();
		localStorage.clear();
		window.__keySafety = { calls: [], seeks: [], muteWrites: [], time: 30, muted: false };
		HTMLMediaElement.prototype.play = function () {
			window.__keySafety.calls.push('play');
			return new Promise<void>(() => {});
		};
		HTMLMediaElement.prototype.pause = function () { window.__keySafety.calls.push('pause'); };
		HTMLMediaElement.prototype.load = function () { window.__keySafety.calls.push('load'); };
	});
	await readyPlayer(page);
}

async function readyPlayer(page: Page) {
	await page.goto('/demo/player', { waitUntil: 'domcontentloaded' });
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	// Observable interaction proves hydration, rather than assuming SSR is ready.
	await page.getByTestId('shortcuts-button').click();
	await expect(page.getByTestId('shortcuts-dialog')).toBeVisible();
	await page.getByTestId('shortcuts-close').click();
	await expect(page.getByTestId('shortcuts-dialog')).toHaveCount(0);
	await media(page);
}

async function media(page: Page, seed: MediaSeed = {}) {
	await page.getByTestId('demo-audio').evaluate((node: HTMLAudioElement, input: MediaSeed) => {
		const probe = window.__keySafety;
		probe.time = input.time ?? 30;
		probe.muted = false;
		Object.defineProperties(node, {
			duration: { configurable: true, get: () => {
				if (input.throws === 'duration') throw new DOMException('synthetic duration refusal');
				return input.duration ?? 120;
			} },
			readyState: { configurable: true, value: input.readyState ?? 1 },
			currentTime: { configurable: true, get: () => {
				if (input.throws === 'get-time') throw new DOMException('synthetic position refusal');
				return probe.time;
			}, set: (value: number) => {
				probe.seeks.push(value);
				if (input.throws === 'set-time') throw new DOMException('synthetic seek refusal');
				probe.time = value;
			} },
			muted: { configurable: true, get: () => probe.muted, set: (value: boolean) => {
				probe.muteWrites.push(value); probe.muted = value;
			} }
		});
	}, seed);
}

async function frame(page: Page) {
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => resolve())));
}

async function snapshot(page: Page) {
	return page.evaluate(() => ({
		...window.__keySafety,
		status: document.querySelector('[data-testid="player-status"]')?.textContent,
		track: document.querySelector('[data-testid="now-playing"]')?.textContent,
		queue: [...document.querySelectorAll('[data-testid="queue-item"]')].map((el) => el.textContent),
		dialogs: document.querySelectorAll('[data-testid="shortcuts-dialog"]').length
	}));
}

type EventSeed = { key: string; isComposing?: boolean; keyCode?: number; cancelled?: boolean;
	ctrlKey?: boolean; altKey?: boolean; metaKey?: boolean; repeat?: boolean };
async function dispatch(page: Page, selector: string, input: EventSeed) {
	return page.locator(selector).evaluate((node, seed: EventSeed) => {
		const event = new KeyboardEvent('keydown', { ...seed, bubbles: true, cancelable: true });
		if (seed.cancelled) event.preventDefault();
		let preventCalls = 0;
		const original = event.preventDefault.bind(event);
		event.preventDefault = () => { preventCalls += 1; original(); };
		node.dispatchEvent(event);
		return { prevented: event.defaultPrevented, preventCalls };
	}, input);
}

// One test per context; every key gets a fresh document so an early erroneous
// navigation/dialog cannot hide a later erroneous seek/mute. Soft assertions
// collect the complete matrix, with no skips or implementation-aware fallbacks.
for (const context of [
	{ id: 'select', html: '<select id="keysafe-target"><option>First</option><option>Next</option></select>' },
	{ id: 'option', html: '<select><option id="keysafe-target">First</option></select>' },
	{ id: 'input', html: '<input id="keysafe-target" aria-label="Acceptance input">' },
	{ id: 'textarea', html: '<textarea id="keysafe-target" aria-label="Acceptance text"></textarea>' },
	{ id: 'editable-child', html: '<div contenteditable="true"><span id="keysafe-target">edit here</span></div>' },
	{ id: 'plaintext-child', html: '<div contenteditable="plaintext-only"><span id="keysafe-target">edit here</span></div>' },
	{ id: 'range', html: '<input id="keysafe-target" type="range" aria-label="Acceptance range">' },
	{ id: 'checkbox', html: '<input id="keysafe-target" type="checkbox" aria-label="Acceptance checkbox">' }
]) {
	test(`KEYSAFE-B01 native ${context.id}: every transport/help key is inert`, async ({ page }) => {
		await openPlayer(page);
		for (const [index, key] of KEYS.entries()) {
			if (index) await readyPlayer(page);
			// A middle track gives both n and p a real destination, avoiding a
			// vacuous previous-at-start guard. Reset media after replacement.
			await page.getByTestId('player-next').click();
			await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
			await media(page);
			await page.locator('main').evaluate((host, html) => host.insertAdjacentHTML('beforeend', html), context.html);
			const before = await snapshot(page);
			const event = await dispatch(page, '#keysafe-target', { key });
			await frame(page);
			expect.soft(event, `${context.id} key=${key}: native default must remain available`).toEqual({ prevented: false, preventCalls: 0 });
			expect.soft(await snapshot(page), `${context.id} key=${key}: zero player effects`).toEqual(before);
		}
	});
}

for (const mode of ['cancelled', 'composing', 'legacy-229'] as const) {
	test(`KEYSAFE-B02 ${mode}: no player effects or additional default suppression`, async ({ page }) => {
		await openPlayer(page);
		for (const [index, key] of KEYS.entries()) {
			if (index) await readyPlayer(page);
			// A middle track gives both n and p a real destination, avoiding a
			// vacuous previous-at-start guard. Reset media after replacement.
			await page.getByTestId('player-next').click();
			await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
			await media(page);
			const before = await snapshot(page);
			const event = await dispatch(page, 'body', { key,
				...(mode === 'cancelled' ? { cancelled: true } : {}),
				...(mode === 'composing' ? { isComposing: true } : {}),
				...(mode === 'legacy-229' ? { keyCode: 229 } : {})
			});
			await frame(page);
			expect.soft(event, `${mode} key=${key}: respect prior key ownership`).toEqual({ prevented: mode === 'cancelled', preventCalls: 0 });
			expect.soft(await snapshot(page), `${mode} key=${key}: zero player effects`).toEqual(before);
		}
	});
}

for (const scheme of ['light', 'dark'] as const) {
	test(`KEYSAFE-B03 native Appearance keyboard at 360px in ${scheme}`, async ({ page }, testInfo) => {
		await page.setViewportSize({ width: 360, height: 800 });
		await page.emulateMedia({ colorScheme: scheme });
		await openPlayer(page);
		const selector = page.getByRole('combobox', { name: 'Appearance', exact: true });
		await expect(selector).toBeVisible();
		const before = await snapshot(page);
		await page.evaluate(() => {
			const audit: { key: string; prevented: boolean }[] = [];
			Object.assign(window, { __nativeKeyAudit: audit });
			window.addEventListener('keydown', (event) => audit.push({ key: event.key, prevented: event.defaultPrevented }));
		});
		await selector.focus();
		// Real trusted keyboard input drives native type-ahead and arrow selection.
		await page.keyboard.press('Home');
		await page.keyboard.press('d');
		await expect(selector).toHaveValue('dark');
		await page.keyboard.press('ArrowUp');
		await expect(selector).toHaveValue('light');
		await page.keyboard.press('Space');
		await page.keyboard.press('Escape');
		await page.keyboard.press('m');
		await frame(page);
		expect.soft(await snapshot(page), 'native selector must not mute or change transport').toEqual(before);
		const events = await page.evaluate(() => (window as unknown as { __nativeKeyAudit: { key: string; prevented: boolean }[] }).__nativeKeyAudit);
		expect.soft(events.length).toBeGreaterThanOrEqual(4);
		expect.soft(events.filter((event) => event.prevented), 'native type-ahead/Space/arrows must not be cancelled by player').toEqual([]);
		await expect(selector).toBeFocused();
		await selector.selectOption(scheme);
		await testInfo.attach(`native-selector-${scheme}`, { body: await page.screenshot(), contentType: 'image/png' });
	});
}

test('KEYSAFE-B04 composing input text remains editable without transport/help effects', async ({ page }) => {
	await openPlayer(page);
	await page.locator('main').evaluate((host) => host.insertAdjacentHTML('beforeend', '<input id="keysafe-input" aria-label="Composition acceptance input">'));
	const input = page.locator('#keysafe-input');
	await input.focus();
	const before = await snapshot(page);
	await input.evaluate((node) => node.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true, data: '' })));
	for (const key of KEYS) {
		expect(await dispatch(page, '#keysafe-input', { key, isComposing: true })).toEqual({ prevented: false, preventCalls: 0 });
	}
	await input.evaluate((node: HTMLInputElement) => {
		node.value = '日本語';
		node.dispatchEvent(new InputEvent('input', { bubbles: true, data: '日本語', inputType: 'insertCompositionText', isComposing: true }));
		node.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true, data: '日本語' }));
	});
	await expect(input).toHaveValue('日本語');
	await page.keyboard.type('knpjlm?');
	await expect(input).toHaveValue('日本語knpjlm?');
	expect(await snapshot(page)).toEqual(before);
});

for (const sample of [
	{ id: 'forward', key: 'l', time: 30.5, duration: 120, expected: 40.5 },
	{ id: 'back', key: 'j', time: 30.5, duration: 120, expected: 20.5 },
	{ id: 'clamp-start', key: 'j', time: 3, duration: 120, expected: 0 },
	{ id: 'clamp-end', key: 'l', time: 115, duration: 120, expected: 120 }
]) {
	test(`KEYSAFE-B05 ${sample.id}: j/l and visible controls make the same safe seek`, async ({ page }) => {
		await openPlayer(page);
		await media(page, sample);
		const before = await snapshot(page);
		await dispatch(page, 'body', { key: sample.key });
		await frame(page);
		const shortcut = await snapshot(page);
		expect(shortcut.time).toBe(sample.expected);
		expect(shortcut.seeks).toEqual([...before.seeks, sample.expected]);
		expect({ ...shortcut, time: before.time, seeks: before.seeks }).toEqual(before);
		await media(page, sample);
		await page.getByTestId(sample.key === 'j' ? 'player-seek-back' : 'player-seek-forward').click();
		await frame(page);
		expect((await snapshot(page)).time).toBe(sample.expected);
	});
}

for (const seed of [
	{ id: 'unknown-duration', duration: NaN },
	{ id: 'infinite-duration', duration: Infinity },
	{ id: 'zero-duration', duration: 0 },
	{ id: 'negative-duration', duration: -1 },
	{ id: 'unknown-position', time: NaN },
	{ id: 'infinite-position', time: Infinity },
	{ id: 'negative-position', time: -1 },
	// A resource without metadata has no finite duration. Do not impose a
	// readyState-only rule on finite media (neighboring jump fixtures omit it).
	{ id: 'unloaded-media', readyState: 0, duration: NaN }
]) {
	test(`KEYSAFE-B06 ${seed.id}: j/l cannot write media time`, async ({ page }) => {
		await openPlayer(page);
		await media(page, seed);
		const before = await snapshot(page);
		for (const key of ['j', 'l']) await dispatch(page, 'body', { key });
		await frame(page);
		expect(await snapshot(page), 'invalid/unloaded media is a no-op, not a fabricated seek').toEqual(before);
	});
}

for (const throws of ['duration', 'get-time', 'set-time'] as const) {
	test(`KEYSAFE-B07 ${throws}: media refusal cannot escape keyboard handler`, async ({ page }) => {
		await openPlayer(page);
		await media(page, { throws });
		const errors: string[] = [];
		page.on('pageerror', (error) => errors.push(error.message));
		const before = await snapshot(page);
		for (const key of ['j', 'l']) await dispatch(page, 'body', { key });
		await frame(page);
		expect.soft(errors, 'media exceptions must be contained').toEqual([]);
		const after = await snapshot(page);
		expect(after.time).toBe(before.time);
		expect({ ...after, seeks: before.seeks }).toEqual(before);
		if (throws !== 'set-time') expect(after.seeks).toEqual(before.seeks);
	});
}

test('KEYSAFE-B08 track replacement and stale media never redirect shortcut seeks', async ({ page }) => {
	await openPlayer(page);
	// Retain an actual detached element, not an invented source/track-id hook.
	await page.getByTestId('demo-audio').evaluate((node) => Object.assign(window, { __retiredKeyAudio: node }));
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await media(page, { time: 50, duration: 200 });
	await page.evaluate(() => {
		const stale = (window as unknown as { __retiredKeyAudio: HTMLAudioElement }).__retiredKeyAudio;
		Object.defineProperty(stale, 'currentTime', { configurable: true, get: () => 30, set: () => { throw new Error('stale element was sought'); } });
		stale.dispatchEvent(new Event('loadedmetadata'));
	});
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	await dispatch(page, 'body', { key: 'j' });
	await frame(page);
	expect((await snapshot(page)).time).toBe(40);
	expect(await page.evaluate(() => (window as unknown as { __retiredKeyAudio: HTMLAudioElement }).__retiredKeyAudio.currentTime)).toBe(30);
	await page.getByTestId('queue-clear').click();
	// An empty player may retain an inert audio node; DOM removal is not part
	// of this contract. There is no selected live media resource to seek.
	await expect(page.getByTestId('player-status')).toHaveText('Status: empty');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	// If the empty transport retains a node, give it stale usable-looking
	// metadata so an unsafe ref-only seek is observable rather than vacuous.
	if (await page.getByTestId('demo-audio').count()) await media(page);
	const before = await snapshot(page);
	for (const key of ['j', 'l']) await dispatch(page, 'body', { key });
	await frame(page);
	expect(await snapshot(page)).toEqual(before);
	expect(errors).toEqual([]);
});

test('KEYSAFE-B09 modifiers remain inert and repeated normal shortcuts retain existing policy', async ({ page }) => {
	await openPlayer(page);
	const before = await snapshot(page);
	for (const modifier of ['ctrlKey', 'metaKey', 'altKey']) {
		for (const key of KEYS) {
			expect(await dispatch(page, 'body', { key, [modifier]: true })).toEqual({ prevented: false, preventCalls: 0 });
		}
	}
	expect(await snapshot(page)).toEqual(before);
	await dispatch(page, 'body', { key: 'l', repeat: true });
	expect((await snapshot(page)).time).toBe(40);
	await dispatch(page, 'body', { key: 'j', repeat: true });
	expect((await snapshot(page)).time).toBe(30);
});

for (const target of ['body', '[data-testid="seek-storm"]', '[data-testid="demo-audio"]']) {
	test(`KEYSAFE-B10 normal target ${target} retains seek/mute/play and help`, async ({ page }) => {
		await openPlayer(page);
		await dispatch(page, target, { key: 'l' });
		expect((await snapshot(page)).time).toBe(40);
		await dispatch(page, target, { key: 'j' });
		expect((await snapshot(page)).time).toBe(30);
		await dispatch(page, target, { key: 'm' });
		expect((await snapshot(page)).muteWrites).toEqual([true]);
		const before = await snapshot(page);
		await dispatch(page, target, { key: 'k' });
		await frame(page);
		expect((await snapshot(page)).calls).toEqual([...before.calls, 'play']);
		await dispatch(page, target, { key: '?' });
		await expect(page.getByTestId('shortcuts-dialog')).toBeVisible();
	});
}

test('KEYSAFE-B11 help keeps its Tab focus trap and normal Escape focus return', async ({ page }) => {
	await openPlayer(page);
	const opener = page.getByTestId('shortcuts-button');
	await opener.click();
	const dialog = page.getByTestId('shortcuts-dialog');
	await expect(dialog).toBeVisible();
	await expect(page.getByTestId('shortcuts-close')).toBeFocused();
	for (const key of ['Tab', 'Shift+Tab', 'Tab', 'Tab']) {
		await page.keyboard.press(key);
		await expect.poll(() => dialog.evaluate((node) => node.contains(document.activeElement))).toBe(true);
	}
	await page.keyboard.press('Escape');
	await expect(dialog).toHaveCount(0);
	await expect(opener).toBeFocused();
});

test('KEYSAFE-B13 help Escape respects cancellation/composition before closing, then normal Escape works', async ({ page }) => {
	await openPlayer(page);
	const opener = page.getByTestId('shortcuts-button');
	await opener.click();
	const dialog = page.getByTestId('shortcuts-dialog');
	await expect(page.getByTestId('shortcuts-close')).toBeFocused();
	const before = await snapshot(page);
	for (const key of ['k', 'n', 'p', 'j', 'l', 'm', '?']) await page.keyboard.press(key);
	expect(await snapshot(page)).toEqual(before);
	for (const seed of [{ cancelled: true }, { isComposing: true }, { keyCode: 229 }]) {
		const event = await dispatch(page, '[data-testid="shortcuts-close"]', { key: 'Escape', ...seed });
		expect.soft(event.preventCalls).toBe(0);
		await expect.soft(dialog).toBeVisible();
		// Reset if an erroneous Escape closed it, so each case is independent.
		if (!(await dialog.count())) await opener.click();
	}
	await page.keyboard.press('Escape');
	await expect(dialog).toHaveCount(0);
	await expect(opener).toBeFocused();
});

test('KEYSAFE-B12 replacement gap refuses seeks on the previous track before render', async ({ page }) => {
	await openPlayer(page);
	const seeks = await page.evaluate(() => {
		const before = window.__keySafety.seeks.length;
		(document.querySelector('[data-testid="player-next"]') as HTMLButtonElement).click();
		// Same-task keyboard events precede the reactive DOM replacement. They
		// must not seek the old resource after queue selection has changed.
		for (const key of ['j', 'l']) document.body.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));
		return window.__keySafety.seeks.slice(before);
	});
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	expect(seeks, 'no write to previous-track media in the replacement gap').toEqual([]);
});
