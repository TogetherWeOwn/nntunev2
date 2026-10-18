import { expect, test, type Page } from '@playwright/test';

/**
 * Pricing plan-blurb contrast.
 *
 * Plan descriptions (`<small>` in the pricing table) are normal text, so
 * they need a WCAG contrast ratio of at least 4.5:1 in both color
 * schemes. Ratios are computed from the rendered computed foreground
 * and the effective (first non-transparent ancestor) background, not
 * from source hex values.
 */

type Rgb = { r: number; g: number; b: number; a: number };

function parseColor(value: string): Rgb {
	const match = value.match(
		/rgba?\((\d+(?:\.\d+)?)[ ,]+(\d+(?:\.\d+)?)[ ,]+(\d+(?:\.\d+)?)(?:[ ,/]+(\d+(?:\.\d+)?))?\)/
	);
	if (!match) throw new Error(`unsupported color: ${value}`);
	return {
		r: Number(match[1]),
		g: Number(match[2]),
		b: Number(match[3]),
		a: match[4] === undefined ? 1 : Number(match[4])
	};
}

function luminance({ r, g, b }: Rgb): number {
	const channel = (v: number) => {
		const c = v / 255;
		return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
	};
	return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

function contrastRatio(foreground: Rgb, background: Rgb): number {
	// Composite a translucent foreground over the effective background.
	const fg: Rgb =
		foreground.a >= 1
			? foreground
			: {
					r: foreground.r * foreground.a + background.r * (1 - foreground.a),
					g: foreground.g * foreground.a + background.g * (1 - foreground.a),
					b: foreground.b * foreground.a + background.b * (1 - foreground.a),
					a: 1
				};
	const [hi, lo] = [luminance(fg), luminance(background)].sort((a, b) => b - a);
	const ratio = (hi! + 0.05) / (lo! + 0.05);
	return ratio;
}

async function blurbColors(
	page: Page
): Promise<Array<{ text: string; foreground: string; background: string }>> {
	return page.evaluate(() => {
		const effectiveBackground = (element: Element): string => {
			let node: Element | null = element;
			while (node) {
				const bg = getComputedStyle(node).backgroundColor;
				if (bg && bg !== 'transparent' && !/rgba\([^)]*,\s*0\)$/.test(bg)) return bg;
				node = node.parentElement;
			}
			return getComputedStyle(document.body).backgroundColor;
		};
		return [...document.querySelectorAll('table tbody th small')].map((element) => ({
			text: element.textContent?.trim() ?? '',
			foreground: getComputedStyle(element).color,
			background: effectiveBackground(element)
		}));
	});
}

for (const colorScheme of ['light', 'dark'] as const) {
	test(`pricing plan blurbs meet 4.5:1 contrast in ${colorScheme} mode`, async ({ page }, testInfo) => {
		await page.emulateMedia({ colorScheme });
		await page.goto('/pricing');

		const blurbs = page.locator('table tbody th small');
		await expect(blurbs.first()).toBeVisible();
		expect(await blurbs.count(), 'pricing table must render plan blurbs').toBeGreaterThan(0);

		for (const { text, foreground, background } of await blurbColors(page)) {
			expect(text, 'blurb text must not be empty').not.toBe('');
			const ratio = contrastRatio(parseColor(foreground), parseColor(background));
			testInfo.annotations.push({
				type: 'contrast',
				description: `${colorScheme}: ${foreground} on ${background} = ${ratio.toFixed(2)}:1 ("${text}")`
			});
			expect(
				ratio,
				`"${text}" in ${colorScheme} mode: ${foreground} on ${background} = ${ratio.toFixed(2)}:1`
			).toBeGreaterThanOrEqual(4.5);
		}
	});
}
