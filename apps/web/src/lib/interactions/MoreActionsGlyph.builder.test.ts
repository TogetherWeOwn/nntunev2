import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import MoreActionsGlyph from './MoreActionsGlyph.svelte';

const sources = import.meta.glob('../player/PlayerQueue.svelte', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;
const library = import.meta.glob('../../routes/library/+page.svelte', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;

describe('shared more-actions glyph', () => {
	it('preserves the original decorative SVG and circle order without a wrapper', () => {
		let body: string = render(MoreActionsGlyph).body;
		while (body.includes('<!--')) {
			const next: string = body.replace(/<!--[\s\S]*?-->/g, '');
			if (next === body) throw new Error('Unterminated SVG comment');
			body = next;
		}
		expect(body.trim()).toBe('<svg aria-hidden="true" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="1"></circle><circle cx="19" cy="12" r="1"></circle><circle cx="5" cy="12" r="1"></circle></svg>');
	});

	it('reuses the same glyph for the queue, Favorites and Library buttons', () => {
		expect(Object.keys(sources)).toHaveLength(1);
		expect(Object.keys(library)).toHaveLength(1);
		expect(Object.values(sources)[0].match(/<MoreActionsGlyph\s*\/>/g)).toHaveLength(1);
		expect(Object.values(library)[0].match(/<MoreActionsGlyph\s*\/>/g)).toHaveLength(2);
	});
});
