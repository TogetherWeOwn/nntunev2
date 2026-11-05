import { afterEach, describe, expect, it } from 'vitest';
import type { Snippet } from 'svelte';
import { onRightPanel, readRightPanel, setRightPanel } from './right-panel.js';

const panel = (() => {}) as unknown as Snippet;

afterEach(() => {
	setRightPanel(null);
});

describe('right-panel slot', () => {
	it('starts closed', () => {
		expect(readRightPanel()).toBeNull();
	});

	it('publishes and closes the slot', () => {
		const seen: Array<unknown> = [];
		const unsubscribe = onRightPanel((next) => seen.push(next));
		setRightPanel(panel);
		setRightPanel(null);
		unsubscribe();
		setRightPanel(panel);
		expect(seen).toEqual([null, panel, null]);
		expect(readRightPanel()).toBe(panel);
	});

	it('a throwing listener breaks nothing', () => {
		onRightPanel(() => {
			throw new Error('listener boom');
		});
		setRightPanel(panel);
		expect(readRightPanel()).toBe(panel);
	});
});
