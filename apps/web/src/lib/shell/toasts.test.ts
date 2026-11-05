import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	clearToasts,
	dismissToast,
	onToasts,
	pushToast,
	readToasts
} from './toasts.js';

afterEach(() => {
	clearToasts();
	vi.useRealTimers();
});

describe('shell toasts', () => {
	it('pushes a message and reads it back', () => {
		pushToast('Removed from Liked', { durationMs: 0 });
		expect(readToasts().map((toast) => toast.message)).toEqual(['Removed from Liked']);
	});

	it('assigns a distinct id per toast', () => {
		pushToast('one', { durationMs: 0 });
		pushToast('two', { durationMs: 0 });
		const [first, second] = readToasts();
		expect(first!.id).not.toBe(second!.id);
	});

	it('notifies subscribers with a snapshot on subscribe and on change', () => {
		const seen: string[][] = [];
		const unsubscribe = onToasts((toasts) => seen.push(toasts.map((toast) => toast.message)));
		pushToast('hello', { durationMs: 0 });
		unsubscribe();
		pushToast('after unsubscribe', { durationMs: 0 });
		expect(seen).toEqual([[], ['hello']]);
	});

	it('dismisses one toast by id and ignores unknown ids', () => {
		pushToast('keep', { durationMs: 0 });
		const dismiss = pushToast('drop', { durationMs: 0 });
		dismissToast(424242);
		expect(readToasts()).toHaveLength(2);
		dismiss();
		expect(readToasts().map((toast) => toast.message)).toEqual(['keep']);
	});

	it('auto-dismisses after the duration', () => {
		vi.useFakeTimers();
		pushToast('fleeting');
		expect(readToasts()).toHaveLength(1);
		vi.advanceTimersByTime(8000);
		expect(readToasts()).toHaveLength(0);
	});

	it('manual dismiss cancels the pending auto-dismiss', () => {
		vi.useFakeTimers();
		const snapshots: number[] = [];
		onToasts((toasts) => snapshots.push(toasts.length));
		const dismiss = pushToast('early');
		dismiss();
		vi.advanceTimersByTime(60_000);
		// Subscribe sees [], push sees [toast], manual dismiss sees []. The
		// advanced clock must not emit anything further.
		expect(snapshots).toEqual([0, 1, 0]);
		expect(readToasts()).toHaveLength(0);
	});

	it('keeps the action on the toast for the layout Undo button', () => {
		let undone = false;
		pushToast('Cleared recent searches', {
			action: { label: 'Undo', run: () => (undone = true) },
			durationMs: 0
		});
		const [toast] = readToasts();
		expect(toast!.action?.label).toBe('Undo');
		toast!.action?.run();
		expect(undone).toBe(true);
	});

	it('clearToasts empties the list and a throwing listener breaks nothing', () => {
		onToasts(() => {
			throw new Error('listener boom');
		});
		pushToast('survives', { durationMs: 0 });
		expect(readToasts()).toHaveLength(1);
		clearToasts();
		expect(readToasts()).toHaveLength(0);
	});
});
