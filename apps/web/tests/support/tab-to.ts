import { writeFile } from 'node:fs/promises';
import { expect, test, type Locator, type Page } from '@playwright/test';

const CANDIDATES = 'a[href], area[href], button, input, select, textarea, iframe, summary, audio[controls], video[controls], [contenteditable]:not([contenteditable="false"]), [tabindex]';

export async function tabTo(page: Page, target: Locator) {
	await expect(target).toBeVisible();
	const observer = await page.evaluateHandle((selector) => {
		const identities = new WeakMap<Element, number>();
		let nextIdentity = 0;
		function sample() {
			let element = document.activeElement;
			while (element?.shadowRoot?.activeElement) element = element.shadowRoot.activeElement;
			if (!element) return { id: 0, body: true, descriptor: 'no active element' };
			if (!identities.has(element)) identities.set(element, ++nextIdentity);
			const root = element.getRootNode() as Document | ShadowRoot;
			const labelledBy = element.getAttribute('aria-labelledby')?.split(/\s+/)
				.map((id) => root.querySelector(`[id="${CSS.escape(id)}"]`)?.textContent ?? '').join(' ');
			const labels = 'labels' in element ? Array.from((element as HTMLInputElement).labels ?? [])
				.map((label) => label.textContent).join(' ') : '';
			const name = element.getAttribute('aria-label') || labelledBy || labels ||
				element.getAttribute('alt') || element.textContent || element.getAttribute('title') || '';
			return { id: identities.get(element)!, body: element === document.body,
				descriptor: `${element.tagName.toLowerCase()} role=${element.getAttribute('role') ?? ''} name=${name.trim().replace(/\s+/g, ' ')} data-testid=${element.getAttribute('data-testid') ?? ''}` };
		}
		function census() {
			let candidates = 0;
			let iframes = 0;
			function visit(root: Document | ShadowRoot) {
				candidates += root.querySelectorAll(selector).length;
				iframes += root.querySelectorAll('iframe').length;
				for (const element of root.querySelectorAll('*')) if (element.shadowRoot) visit(element.shadowRoot);
			}
			visit(document);
			return { candidates, iframes };
		}
		return { sample, census };
	}, CANDIDATES);
	const initial = await observer.evaluate((state) => ({ start: state.sample(), ...state.census() }));
	const bound = Math.max(80, initial.candidates + 2);
	const sequence: typeof initial.start[] = [];
	const seen = new Set<number>();
	let failure: string | null = null;
	let traversalError: unknown;
	let final: { focus: typeof initial.start; candidates: number; iframes: number } | undefined;
	try {
		let reached = await target.evaluate((node) => node === document.activeElement);
		for (let i = 0; i < bound && !reached; i += 1) {
			await page.keyboard.press('Tab');
			const focused = await observer.evaluate((state) => state.sample());
			sequence.push(focused);
			reached = await target.evaluate((node) => node === document.activeElement);
			if (reached) break;
			if (sequence.length > 1 && sequence.at(-2)!.id === focused.id) failure = 'focus stuck';
			else if (sequence.length > 1 && focused.id === initial.start.id) failure = 'target not in tab cycle';
			else if (!focused.body && seen.has(focused.id)) failure = 'focus trap';
			if (failure) break;
			seen.add(focused.id);
		}
		if (!reached && !failure) failure = 'bound exhausted';
		final = await observer.evaluate((state) => ({ focus: state.sample(), ...state.census() }));
		const diagnostic = { start: initial.start, presses: sequence.length, B: bound,
			C_start: initial.candidates, C_end: final.candidates, iframeCount: final.iframes,
			initialIframeCount: initial.iframes, final: final.focus, sequence };
		if (failure) {
			const grew = final.candidates > initial.candidates
				? `; DOM grew during traversal (${initial.candidates}→${final.candidates})` : '';
			throw new Error(`${failure}${grew}; ${JSON.stringify({ ...diagnostic, sequence: sequence.slice(-20) })}`);
		}
		await expect(target, 'control must be reachable by Tab').toBeFocused();
		const visibleFocus = await target.evaluate((node) => {
			const css = getComputedStyle(node);
			return node.matches(':focus-visible') && (
				(css.outlineStyle !== 'none' && parseFloat(css.outlineWidth) > 0) || css.boxShadow !== 'none'
			);
		});
		expect(visibleFocus, 'keyboard focus needs a visible outline or focus ring').toBe(true);
	} catch (error) {
		traversalError = error;
		throw error;
	} finally {
		let observationError: string | null = null;
		try {
			final ??= await observer.evaluate((state) => ({ focus: state.sample(), ...state.census() }));
		} catch (error) {
			observationError = String(error);
		}
		try {
			// CI uploads test-results files, not the dot reporter's in-memory attachment bodies.
			const path = test.info().outputPath(`tab-traversal-${test.info().attachments.length}.json`);
			await writeFile(path, JSON.stringify({
				start: initial.start, presses: sequence.length, B: bound, C_start: initial.candidates,
				C_end: final?.candidates ?? null, initialIframeCount: initial.iframes,
				iframeCount: final?.iframes ?? null, final: final?.focus ?? null, sequence,
				failure, error: traversalError ? String(traversalError) : null, observationError
			}));
			await test.info().attach('tab-traversal', { path, contentType: 'application/json' });
		} catch (error) {
			if (!traversalError) throw error;
		} finally {
			try { await observer.dispose(); } catch (error) { if (!traversalError) throw error; }
		}
	}
}
