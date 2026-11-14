import { afterEach, describe, expect, it, vi } from 'vitest';
import { clipboardWriter } from '../clipboard.js';
import { formatRemainingMs } from '../format.js';

afterEach(() => vi.unstubAllGlobals());

describe('shared clipboard detection', () => {
	it.each([undefined, null, {}, { writeText: 'unavailable' }])('refuses unavailable clipboard %j', (clipboard) => {
		vi.stubGlobal('navigator', { clipboard });
		expect(clipboardWriter()).toBeUndefined();
	});

	it('refuses a throwing clipboard getter', () => {
		vi.stubGlobal('navigator', { get clipboard() { throw new Error('Denied'); } });
		expect(clipboardWriter()).toBeUndefined();
	});

	it('refuses a throwing method getter', () => {
		vi.stubGlobal('navigator', { clipboard: { get writeText() { throw new Error('Denied'); } } });
		expect(clipboardWriter()).toBeUndefined();
	});

	it('reads once without writing, preserves the receiver and discovers each activation afresh', async () => {
		let reads = 0;
		const writeText = vi.fn(function (this: unknown) { expect(this).toBe(clipboard); return Promise.resolve(); });
		const clipboard = { writeText };
		vi.stubGlobal('navigator', { get clipboard() { reads++; return clipboard; } });
		const writer = clipboardWriter();
		expect(reads).toBe(1);
		expect(writer).toBe(clipboard);
		expect(writeText).not.toHaveBeenCalled();
		await writer?.writeText('text');
		expect(writeText).toHaveBeenCalledExactlyOnceWith('text');
		vi.stubGlobal('navigator', { clipboard: undefined });
		expect(clipboardWriter()).toBeUndefined();
	});
});

describe('shared remaining-time formatting', () => {
	it.each([
		[-1000, '0:00'], [0, '0:00'], [0.01, '0:01'], [999, '0:01'],
		[1000, '0:01'], [1001, '0:02'], [59999, '1:00'], [60000, '1:00'],
		[3599000, '59:59'], [3599001, '1:00:00'], [3600000, '1:00:00'], [3661000, '1:01:01']
	])('formats %d ms as %s', (ms, expected) => {
		expect(formatRemainingMs(ms as number)).toBe(expected);
	});
});
