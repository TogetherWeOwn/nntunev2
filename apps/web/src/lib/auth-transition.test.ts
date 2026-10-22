import { describe, expect, it, vi } from 'vitest';
import { notifyLocalAuthTransition, subscribeLocalAuthTransition } from './auth-transition.js';

/**
 * Same-window auth-transition dispatch contract. The security page calls
 * `notifyLocalAuthTransition()` after a qualifying result; every mounted
 * consumer (library, provider, BYOP) resets through the same callback it
 * uses for the delivered cross-tab transition. Both entry points are
 * no-throw; a failing listener or broken callback must never suppress the
 * other local resets.
 */
describe('subscribeLocalAuthTransition', () => {
	it('delivers a later notification to the subscriber', () => {
		const listener = vi.fn();
		const unsubscribe = subscribeLocalAuthTransition(listener);
		try {
			notifyLocalAuthTransition();
			expect(listener).toHaveBeenCalledTimes(1);
		} finally {
			unsubscribe();
		}
	});

	it('stops delivering after unsubscribe', () => {
		const listener = vi.fn();
		const unsubscribe = subscribeLocalAuthTransition(listener);
		unsubscribe();
		notifyLocalAuthTransition();
		expect(listener).not.toHaveBeenCalled();
	});

	it('delivers to every subscriber independently', () => {
		const first = vi.fn();
		const second = vi.fn();
		const offFirst = subscribeLocalAuthTransition(first);
		const offSecond = subscribeLocalAuthTransition(second);
		try {
			notifyLocalAuthTransition();
			expect(first).toHaveBeenCalledTimes(1);
			expect(second).toHaveBeenCalledTimes(1);
		} finally {
			offFirst();
			offSecond();
		}
	});

	it('unsubscribing twice does not throw', () => {
		const unsubscribe = subscribeLocalAuthTransition(() => {});
		unsubscribe();
		expect(() => unsubscribe()).not.toThrow();
	});
});

describe('notifyLocalAuthTransition', () => {
	it('is a no-op without subscribers and never throws', () => {
		expect(() => notifyLocalAuthTransition()).not.toThrow();
	});

	it('isolates a throwing listener so later subscribers still reset', () => {
		const calls: string[] = [];
		const offBad = subscribeLocalAuthTransition(() => {
			calls.push('bad');
			throw new Error('fixture listener failure');
		});
		const offGood = subscribeLocalAuthTransition(() => {
			calls.push('good');
		});
		try {
			expect(() => notifyLocalAuthTransition()).not.toThrow();
			expect(calls).toEqual(['bad', 'good']);
		} finally {
			offBad();
			offGood();
		}
	});

	it('repeated notification settles without duplicating subscriptions', () => {
		const listener = vi.fn();
		const unsubscribe = subscribeLocalAuthTransition(listener);
		try {
			notifyLocalAuthTransition();
			notifyLocalAuthTransition();
			expect(listener).toHaveBeenCalledTimes(2);
		} finally {
			unsubscribe();
		}
	});
});
