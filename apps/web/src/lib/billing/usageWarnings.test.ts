import { describe, expect, it } from 'vitest';
import {
	markUsageWarningsShown,
	pendingUsageWarnings,
	type WarningStorage
} from './usageWarnings';

function memoryStorage(initial: Record<string, string> = {}): WarningStorage {
	const map = new Map(Object.entries(initial));
	return {
		getItem: (key) => map.get(key) ?? null,
		setItem: (key, value) => {
			map.set(key, value);
		}
	};
}

describe('pendingUsageWarnings', () => {
	it('fires nothing below 80%', () => {
		expect(pendingUsageWarnings('2026-11', 0.79, memoryStorage())).toEqual([]);
	});

	it('fires 80 exactly at the threshold, once per period', () => {
		const storage = memoryStorage();
		expect(pendingUsageWarnings('2026-11', 0.8, storage)).toEqual([80]);
		// Still pending until marked (display is what consumes it).
		expect(pendingUsageWarnings('2026-11', 0.81, storage)).toEqual([80]);
		markUsageWarningsShown('2026-11', [80], storage);
		expect(pendingUsageWarnings('2026-11', 0.9, storage)).toEqual([]);
	});

	it('crossing straight to 97% returns both thresholds ascending', () => {
		expect(pendingUsageWarnings('2026-11', 0.97, memoryStorage())).toEqual([80, 95]);
	});

	it('a new period re-arms every threshold', () => {
		const storage = memoryStorage();
		markUsageWarningsShown('2026-11', [80, 95], storage);
		expect(pendingUsageWarnings('2026-11', 0.99, storage)).toEqual([]);
		expect(pendingUsageWarnings('2026-12', 0.99, storage)).toEqual([80, 95]);
	});

	it('fires 95 after 80 was already shown in an earlier visit', () => {
		const storage = memoryStorage();
		markUsageWarningsShown('2026-11', [80], storage);
		expect(pendingUsageWarnings('2026-11', 0.96, storage)).toEqual([95]);
	});

	it('never fires without storage or a period', () => {
		expect(pendingUsageWarnings('2026-11', 0.99, null)).toEqual([]);
		expect(pendingUsageWarnings('', 0.99, memoryStorage())).toEqual([]);
	});

	it('treats throwing storage as already shown (never spams)', () => {
		const broken: WarningStorage = {
			getItem: () => {
				throw new Error('denied');
			},
			setItem: () => {
				throw new Error('denied');
			}
		};
		expect(pendingUsageWarnings('2026-11', 0.99, broken)).toEqual([]);
		expect(() => markUsageWarningsShown('2026-11', [80], broken)).not.toThrow();
	});
});
