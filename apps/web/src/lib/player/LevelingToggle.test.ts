import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import LevelingToggle from './LevelingToggle.svelte';

/**
 * Builder-owned pins for the leveling toggle's accessible contract: a
 * labeled native button (keyboard reachable by construction) whose
 * `aria-pressed` state announces on/off to assistive tech.
 */

function toggleTag(enabled: boolean): string | null {
	const body = render(LevelingToggle, { props: { enabled } }).body;
	return body.match(/<[a-zA-Z][\w-]*\b[^>]*data-testid="leveling-toggle"[^>]*>/)?.[0] ?? null;
}

function toggleText(enabled: boolean): string {
	const body = render(LevelingToggle, { props: { enabled } }).body;
	return body
		.replace(/<[^>]*>/g, ' ')
		.replace(/\s+/g, ' ')
		.trim();
}

describe('LevelingToggle accessible contract', () => {
	it('renders a labeled button with the pressed state announced', () => {
		for (const enabled of [true, false]) {
			const tag = toggleTag(enabled);
			expect(tag).not.toBeNull();
			expect(tag).toMatch(/^<button\b/);
			expect(tag).toContain(`aria-pressed="${enabled ? 'true' : 'false'}"`);
			expect(tag).toMatch(/aria-label="Loudness leveling"/);
		}
	});

	it('shows the on/off state as visible text', () => {
		expect(toggleText(true)).toContain('Leveling: On');
		expect(toggleText(false)).toContain('Leveling: Off');
	});
});
