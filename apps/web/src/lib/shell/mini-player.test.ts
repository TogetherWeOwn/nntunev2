import { afterEach, describe, expect, it } from 'vitest';
import {
	clearMiniPlayerHint,
	onMiniPlayerHint,
	readMiniPlayerHint,
	setMiniPlayerHint,
	type MiniPlayerHint
} from './mini-player.js';

const HINT: MiniPlayerHint = { trackId: 'track-1', title: 'Harbor Song', artist: 'Echo' };

afterEach(() => {
	clearMiniPlayerHint();
});

describe('mini-player hint', () => {
	it('starts empty', () => {
		expect(readMiniPlayerHint()).toBeNull();
	});

	it('publishes a selection to subscribers, latest wins', () => {
		const seen: Array<MiniPlayerHint | null> = [];
		const unsubscribe = onMiniPlayerHint((hint) => seen.push(hint));
		setMiniPlayerHint(HINT);
		setMiniPlayerHint({ trackId: 'track-2', title: 'Beta Song', artist: null });
		unsubscribe();
		setMiniPlayerHint(HINT);
		expect(seen).toEqual([
			null,
			HINT,
			{ trackId: 'track-2', title: 'Beta Song', artist: null }
		]);
		expect(readMiniPlayerHint()).toEqual(HINT);
	});

	it('clears back to empty and stays silent when already empty', () => {
		const seen: Array<MiniPlayerHint | null> = [];
		onMiniPlayerHint((hint) => seen.push(hint));
		setMiniPlayerHint(HINT);
		clearMiniPlayerHint();
		clearMiniPlayerHint();
		expect(seen).toEqual([null, HINT, null]);
		expect(readMiniPlayerHint()).toBeNull();
	});

	it('a throwing listener breaks nothing', () => {
		const seen: Array<MiniPlayerHint | null> = [];
		onMiniPlayerHint(() => {
			throw new Error('listener boom');
		});
		onMiniPlayerHint((hint) => seen.push(hint));
		setMiniPlayerHint(HINT);
		expect(readMiniPlayerHint()).toEqual(HINT);
		expect(seen).toEqual([null, HINT]);
	});
});
