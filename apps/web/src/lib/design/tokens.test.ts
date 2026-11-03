import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

/**
 * Design-token acceptance for the W5 token layer.
 *
 * Protected contract: the nine color tokens keep their names and values
 * byte-for-byte in every palette (dark default, light media query, and
 * both explicit appearance overrides). Anything that wants a different
 * value changes these pins first, in the open, with the reason recorded.
 *
 * Second half: every documented text pair meets WCAG AA (4.5:1) and every
 * focus pair meets 3:1, computed from the shipped file — not from copies
 * of the values. Semantic status text uses the `-text` tokens, never the
 * plain good/bad tokens alone; good/bad on their own are graphic and icon
 * colour held to 3:1.
 */

// The shipped stylesheet bytes: this workspace's Vite 8 pipeline resolves
// `?raw`/`?inline` CSS imports to an empty string (probed), so the test
// reads the file directly. Runtime is node under vitest; the matching
// ambient declaration in `./node-fs.d.ts` covers `svelte-check`, since
// the web package carries no @types/node by convention.
const css = readFileSync(new URL('./tokens.css', import.meta.url), 'utf8');

type VarMap = Map<string, string>;

function parseDeclarations(body: string): VarMap {
	const vars: VarMap = new Map();
	for (const match of body.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
		vars.set(match[1], match[2].trim());
	}
	return vars;
}

/** Top-level blocks with brace matching, in file order. */
function topBlocks(source: string): Array<{ header: string; body: string }> {
	const clean = source.replace(/\/\*[\s\S]*?\*\//g, '');
	const blocks: Array<{ header: string; body: string }> = [];
	let i = 0;
	while (i < clean.length) {
		const open = clean.indexOf('{', i);
		if (open < 0) break;
		const header = clean.slice(i, open).trim();
		let depth = 0;
		let j = open;
		for (; j < clean.length; j++) {
			if (clean[j] === '{') depth++;
			if (clean[j] === '}') {
				depth--;
				if (depth === 0) break;
			}
		}
		blocks.push({ header, body: clean.slice(open + 1, j) });
		i = j + 1;
	}
	return blocks;
}

/** Merge in cascade order; later maps win. Resolves var(--x) references. */
function resolveAll(maps: VarMap[]): VarMap {
	const merged: VarMap = new Map();
	for (const m of maps) for (const [k, v] of m) merged.set(k, v);
	const resolved: VarMap = new Map();
	const visit = (key: string, seen: string[]): string => {
		if (resolved.has(key)) return resolved.get(key)!;
		if (seen.includes(key)) throw new Error(`cyclic token reference: ${[...seen, key].join(' -> ')}`);
		const raw = merged.get(key) ?? '';
		const out = raw.replace(/var\((--[\w-]+)\)/g, (_, ref: string) => visit(ref, [...seen, key]));
		resolved.set(key, out);
		return out;
	};
	for (const key of merged.keys()) visit(key, []);
	return resolved;
}

const plain: VarMap[] = [];
const mediaLight: VarMap[] = [];
const attrLight: VarMap[] = [];
const attrDark: VarMap[] = [];
for (const block of topBlocks(css)) {
	if (block.header === ':root') plain.push(parseDeclarations(block.body));
	else if (block.header.startsWith('@media')) {
		for (const inner of block.body.matchAll(/:root\s*\{([^}]*)\}/g)) {
			mediaLight.push(parseDeclarations(inner[1]));
		}
	} else if (block.header === ":root[data-appearance='light']") {
		attrLight.push(parseDeclarations(block.body));
	} else if (block.header === ":root[data-appearance='dark']") {
		attrDark.push(parseDeclarations(block.body));
	}
}

// Same cascade the browser applies: plain roots in file order (dark
// defaults), then the light overlays, then the forced-appearance wins.
const dark = resolveAll([...plain, ...attrDark]);
const light = resolveAll([...plain, ...mediaLight, ...attrLight]);

const roots = plain;

function channel(c: number): number {
	const s = c / 255;
	return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
}

function luminance(hex: string): number {
	const m = hex.match(/^#([0-9a-fA-F]{6})$/);
	if (!m) throw new Error(`not an opaque hex color: ${hex}`);
	const n = parseInt(m[1], 16);
	return 0.2126 * channel((n >> 16) & 255) + 0.7152 * channel((n >> 8) & 255) + 0.0722 * channel(n & 255);
}

function contrast(a: string, b: string): number {
	const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
	return (hi + 0.05) / (lo + 0.05);
}

// The nine protected color tokens, with the exact shipped values per theme.
const PROTECTED: Record<string, { dark: string; light: string }> = {
	'--nt-bg': { dark: '#0b0d12', light: '#f7f8fb' },
	'--nt-bg-raised': { dark: '#141824', light: '#ffffff' },
	'--nt-fg': { dark: '#f2f4f8', light: '#141824' },
	'--nt-fg-muted': { dark: '#a7b0c2', light: '#4d5670' },
	'--nt-accent': { dark: '#7c9aff', light: '#2f4fe0' },
	'--nt-accent-fg': { dark: '#0b0d12', light: '#ffffff' },
	'--nt-border': { dark: '#2a3247', light: '#dfe3ee' },
	'--nt-good': { dark: '#57d98d', light: '#1a7f45' },
	'--nt-bad': { dark: '#ff7a7a', light: '#c0262d' }
};

describe('design tokens: protected nine', () => {
	for (const [name, values] of Object.entries(PROTECTED)) {
		it(`${name} keeps its dark and light values in every palette`, () => {
			expect(roots[0].get(name), `original :root ${name}`).toBe(values.dark);
			expect(dark.get(name), `resolved dark ${name}`).toBe(values.dark);
			expect(light.get(name), `resolved light ${name}`).toBe(values.light);
		});
	}
});

describe('design tokens: new layer presence', () => {
	const surface = [
		'--nt-surface-pane',
		'--nt-surface-raised',
		'--nt-surface-hover',
		'--nt-surface-press',
		'--nt-surface-tint',
		'--nt-surface-tint-hover',
		'--nt-surface-tint-press',
		'--nt-fg-subtle',
		'--nt-scrim',
		'--nt-shadow-overlay',
		'--nt-hero-scrim'
	];
	for (const name of surface) {
		it(`${name} resolves in both themes`, () => {
			expect(dark.get(name), `dark ${name}`).toMatch(/\S/);
			expect(light.get(name), `light ${name}`).toMatch(/\S/);
		});
	}

	it('semantic text and focus tokens resolve in both themes', () => {
		for (const name of [
			'--nt-good-text',
			'--nt-bad-text',
			'--nt-focus-ring',
			'--nt-focus-ring-width',
			'--nt-focus-ring-offset'
		]) {
			expect(dark.get(name), `dark ${name}`).toMatch(/\S/);
			expect(light.get(name), `light ${name}`).toMatch(/\S/);
		}
	});

	it('focus ring resolves to the accent color with fixed geometry', () => {
		expect(dark.get('--nt-focus-ring')).toBe(dark.get('--nt-accent'));
		expect(light.get('--nt-focus-ring')).toBe(light.get('--nt-accent'));
		expect(dark.get('--nt-focus-ring-width')).toBe('2px');
		expect(dark.get('--nt-focus-ring-offset')).toBe('2px');
	});

	it('type scale, space aliases, radii, sizes and motion resolve', () => {
		const names = [
			'--nt-text-2xs',
			'--nt-text-xs',
			'--nt-text-sm',
			'--nt-text-base',
			'--nt-text-lg',
			'--nt-text-xl',
			'--nt-text-2xl',
			'--nt-text-3xl',
			'--nt-text-4xl',
			'--nt-text-display',
			'--nt-leading-sm',
			'--nt-leading-base',
			'--nt-leading-3xl',
			'--nt-leading-4xl',
			'--nt-leading-display',
			'--nt-weight-body',
			'--nt-weight-strong',
			'--nt-weight-display',
			'--nt-space-px-2',
			'--nt-space-px-4',
			'--nt-space-px-6',
			'--nt-space-px-8',
			'--nt-space-px-12',
			'--nt-space-px-16',
			'--nt-space-px-20',
			'--nt-space-px-24',
			'--nt-space-px-32',
			'--nt-space-px-40',
			'--nt-space-px-48',
			'--nt-space-px-64',
			'--nt-radius-xs',
			'--nt-radius-sm',
			'--nt-radius-md',
			'--nt-radius-pane',
			'--nt-radius-dialog',
			'--nt-radius-pill',
			'--nt-control-sm',
			'--nt-control',
			'--nt-control-lg',
			'--nt-icon',
			'--nt-topbar-h',
			'--nt-player-h',
			'--nt-rail-w',
			'--nt-rail-w-min',
			'--nt-rail-w-max',
			'--nt-rail-w-icons',
			'--nt-panel-w',
			'--nt-panel-w-min',
			'--nt-panel-w-max',
			'--nt-shell-gap',
			'--nt-content-max',
			'--nt-target-coarse',
			'--nt-z-rail',
			'--nt-z-above-panes',
			'--nt-z-player',
			'--nt-z-above-player',
			'--nt-z-dialog',
			'--nt-z-popover',
			'--nt-z-toast',
			'--nt-z-skip-link',
			'--nt-dur-micro',
			'--nt-dur-fast',
			'--nt-dur-fade',
			'--nt-dur-exit',
			'--nt-dur-enter',
			'--nt-dur-panel',
			'--nt-dur-expand',
			'--nt-ease-productive',
			'--nt-ease-enter',
			'--nt-ease-exit'
		];
		for (const name of names) {
			expect(dark.get(name), `dark ${name}`).toMatch(/\S/);
			expect(light.get(name), `light ${name}`).toMatch(/\S/);
		}
	});

	it('self-hosted typeface: three Figtree weights with self paths and swap', () => {
		const faces = [...css.matchAll(/@font-face\s*\{([^}]*)\}/g)].map((m) => m[1]);
		expect(faces, 'three @font-face blocks').toHaveLength(3);
		for (const weight of ['400', '700', '800']) {
			const face = faces.find((b) => new RegExp(`font-weight:\\s*${weight}\\b`).test(b));
			expect(face, `weight ${weight} declared`).toBeTruthy();
			expect(face).toMatch(/font-family:\s*'Figtree'/);
			expect(face).toMatch(/font-display:\s*swap/);
			expect(face).toMatch(new RegExp(`src:\\s*url\\('/fonts/figtree-latin-${weight}-normal\\.woff2'\\)`));
		}
		expect(dark.get('--nt-font')).toMatch(/^'Figtree',\s*system-ui/);
	});
});

describe('design tokens: contrast gates', () => {
	const textPairs: Array<[string, string, string]> = [
		['dark fg on bg', '--nt-fg', '--nt-bg'],
		['dark fg on pane', '--nt-fg', '--nt-surface-pane'],
		['dark fg on hover', '--nt-fg', '--nt-surface-hover'],
		['dark muted on bg', '--nt-fg-muted', '--nt-bg'],
		['dark muted on pane', '--nt-fg-muted', '--nt-surface-pane'],
		['dark subtle on pane', '--nt-fg-subtle', '--nt-surface-pane'],
		['dark button text', '--nt-accent-fg', '--nt-accent'],
		['dark good text on bg', '--nt-good-text', '--nt-bg'],
		['dark good text on pane', '--nt-good-text', '--nt-surface-pane'],
		['dark bad text on bg', '--nt-bad-text', '--nt-bg'],
		['dark bad text on pane', '--nt-bad-text', '--nt-surface-pane'],
		['light fg on bg', '--nt-fg', '--nt-bg'],
		['light fg on pane', '--nt-fg', '--nt-surface-pane'],
		['light fg on hover', '--nt-fg', '--nt-surface-hover'],
		['light muted on bg', '--nt-fg-muted', '--nt-bg'],
		['light muted on pane', '--nt-fg-muted', '--nt-surface-pane'],
		['light subtle on pane', '--nt-fg-subtle', '--nt-surface-pane'],
		['light button text', '--nt-accent-fg', '--nt-accent'],
		['light good text on bg', '--nt-good-text', '--nt-bg'],
		['light good text on pane', '--nt-good-text', '--nt-surface-pane'],
		['light bad text on bg', '--nt-bad-text', '--nt-bg'],
		['light bad text on pane', '--nt-bad-text', '--nt-surface-pane']
	];
	for (const [label, fg, bg] of textPairs) {
		it(`text ${label} meets 4.5:1`, () => {
			const theme = label.startsWith('dark') ? dark : light;
			const ratio = contrast(theme.get(fg)!, theme.get(bg)!);
			expect(ratio, `${label}: ${theme.get(fg)} on ${theme.get(bg)} = ${ratio.toFixed(2)}`).toBeGreaterThanOrEqual(
				4.5
			);
		});
	}

	const focusPairs: Array<[string, string, string]> = [
		['dark focus on bg', '--nt-focus-ring', '--nt-bg'],
		['dark focus on pane', '--nt-focus-ring', '--nt-surface-pane'],
		['dark focus on raised', '--nt-focus-ring', '--nt-surface-raised'],
		['dark focus on hover', '--nt-focus-ring', '--nt-surface-hover'],
		['light focus on bg', '--nt-focus-ring', '--nt-bg'],
		['light focus on pane', '--nt-focus-ring', '--nt-surface-pane'],
		['light focus on raised', '--nt-focus-ring', '--nt-surface-raised'],
		['light focus on hover', '--nt-focus-ring', '--nt-surface-hover']
	];
	for (const [label, fg, bg] of focusPairs) {
		it(`focus ${label} meets 3:1`, () => {
			const theme = label.startsWith('dark') ? dark : light;
			const ratio = contrast(theme.get(fg)!, theme.get(bg)!);
			expect(ratio, `${label}: ${theme.get(fg)} on ${theme.get(bg)} = ${ratio.toFixed(2)}`).toBeGreaterThanOrEqual(
				3
			);
		});
	}

	const graphicPairs: Array<[string, string, string]> = [
		['dark good on bg', '--nt-good', '--nt-bg'],
		['dark good on pane', '--nt-good', '--nt-surface-pane'],
		['dark bad on bg', '--nt-bad', '--nt-bg'],
		['dark bad on pane', '--nt-bad', '--nt-surface-pane'],
		['light good on bg', '--nt-good', '--nt-bg'],
		['light good on pane', '--nt-good', '--nt-surface-pane'],
		['light bad on bg', '--nt-bad', '--nt-bg'],
		['light bad on pane', '--nt-bad', '--nt-surface-pane']
	];
	for (const [label, fg, bg] of graphicPairs) {
		it(`graphic ${label} meets 3:1 (never body text alone)`, () => {
			const theme = label.startsWith('dark') ? dark : light;
			const ratio = contrast(theme.get(fg)!, theme.get(bg)!);
			expect(ratio, `${label}: ${theme.get(fg)} on ${theme.get(bg)} = ${ratio.toFixed(2)}`).toBeGreaterThanOrEqual(
				3
			);
		});
	}
});
