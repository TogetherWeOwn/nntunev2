import { afterEach, expect, it, vi } from 'vitest';
import { AUTH_GENERATION_KEY, runAuthMutation } from './auth-fence.js';
import { notifyLocalAuthTransition, subscribeLocalAuthTransition } from './auth-transition.js';
import { attributionInventory, validateAttributionInventory } from './attributions/index.js';
import { menuPosition } from './interactions/context-menu.js';
import { pointerQueueDrag } from './interactions/pointer-queue-drag.js';
import { droppedTrackDrag, endTrackDrag, startTrackDrag } from './interactions/track-drag.js';
import { clearMiniPlayerHint, readMiniPlayerHint, setMiniPlayerHint } from './shell/mini-player.js';
import { readRightPanel, setRightPanel } from './shell/right-panel.js';
import { clearToasts, pushToast, readToasts, TOAST_DURATION_MS } from './shell/toasts.js';
import { decodeBase64Url, serializePublicKeyCredential, toCreationOptions, toRequestOptions } from './webauthn.js';

afterEach(() => {
	endTrackDrag();
	clearMiniPlayerHint();
	setRightPanel(null);
	clearToasts();
	vi.useRealTimers();
	vi.unstubAllGlobals();
	vi.doUnmock('./attributions/contract.js');
});

it('refuses a logout fallback after the auth generation changes', async () => {
	const values = new Map([[AUTH_GENERATION_KEY, 'original']]);
	vi.stubGlobal('window', { localStorage: {
		setItem: (key: string, value: string) => values.set(key, value),
		getItem: (key: string) => values.get(key) ?? null,
		removeItem: (key: string) => values.delete(key)
	} });
	vi.stubGlobal('navigator', { locks: { request: async () => {
		values.set(AUTH_GENERATION_KEY, 'replacement');
		throw new DOMException('Lock unavailable', 'SecurityError');
	} } });
	const dispatch = vi.fn(async () => true);
	const stale = new Error('stale logout');
	await expect(runAuthMutation(new AbortController().signal, dispatch, { stale })).rejects.toBe(stale);
	expect(dispatch).not.toHaveBeenCalled();
	expect([...values]).toEqual([[AUTH_GENERATION_KEY, 'replacement']]);
});

it('refuses a revocation if a newer auth attempt starts during admission', async () => {
	const gate = Promise.withResolvers<string[]>();
	const generation = 'original';
	vi.stubGlobal('window', { localStorage: {
		getItem: () => generation, setItem: () => {}
	} });
	vi.stubGlobal('navigator', { locks: { request: (
		_name: string, options: LockOptions, callback: (lock: object) => unknown
	) => options.ifAvailable ? callback({}) : Promise.reject(options.signal?.reason) } });
	vi.stubGlobal('caches', { keys: () => gate.promise });
	const dispatch = vi.fn(async () => true);
	const stale = new Error('stale revocation');
	const revocation = runAuthMutation(new AbortController().signal, dispatch, {
		stale, revocation: { generation }
	});
	const newer = new AbortController();
	newer.abort();
	await expect(runAuthMutation(newer.signal, dispatch)).rejects.toBe(newer.signal.reason);
	gate.resolve([]);
	await expect(revocation).rejects.toBe(stale);
	expect(dispatch).not.toHaveBeenCalled();
});

it('skips listeners retired and resubscribed during an auth transition', () => {
	const listener = vi.fn();
	let offReplacement = () => {};
	const offFirst = subscribeLocalAuthTransition(() => {
		offSecond();
		offReplacement = subscribeLocalAuthTransition(listener);
	});
	const offSecond = subscribeLocalAuthTransition(listener);
	try {
		notifyLocalAuthTransition();
		expect(listener).not.toHaveBeenCalled();
		offFirst();
		notifyLocalAuthTransition();
		expect(listener).toHaveBeenCalledOnce();
	} finally {
		offFirst();
		offSecond();
		offReplacement();
	}
});

it.each(['_x', 'YQ==', 'YQ  ', '+w'])('rejects noncanonical base64url %j', (value) => {
	expect(() => decodeBase64Url(value)).toThrow();
});

it.each(['', 'A'.repeat(44), 'YQ='])('rejects an invalid challenge %j', (challenge) => {
	expect(() => toRequestOptions({ challenge })).toThrow();
	expect(() => toCreationOptions({ challenge, rp: { name: 'Example' },
		user: { id: 'AQ', name: 'example', displayName: 'Example' }, pubKeyCredParams: [] })).toThrow();
});

it('preserves a correctly padded challenge at the browser boundary', () => {
	expect(toRequestOptions({ challenge: 'YQ==' }).challenge).toEqual(new Uint8Array([97]));
});

it.each([
	['empty id', [{ id: '' }]],
	['wrong type', [{ id: 'AQ', type: 'password' as PublicKeyCredentialType }]],
	['too many entries', Array(65).fill({ id: 'AQ' })],
	['sparse list', new Array(1)]
] as const)('rejects a descriptor list with %s', (_label, descriptors) => {
	expect(() => toRequestOptions({ challenge: 'AQ', allowCredentials: descriptors.slice() })).toThrow();
});

it.each([0, 16_385])('rejects an invalid browser credential buffer of %d bytes', (size) => {
	const serialize = (authenticatorData: ArrayBuffer) => serializePublicKeyCredential({
		id: 'AQ', rawId: new Uint8Array([1]).buffer, type: 'public-key',
		response: { clientDataJSON: new Uint8Array([2]).buffer, authenticatorData,
			signature: new Uint8Array([3]).buffer, userHandle: null },
		getClientExtensionResults: () => ({})
	} as unknown as PublicKeyCredential);
	expect(() => serialize(new ArrayBuffer(size))).toThrow();
	expect(serialize(new ArrayBuffer(1)).id).toBe('AQ');
});

it('bounds attribution entry counts, evidence lists and field lengths', () => {
	const entry = attributionInventory.entries[0];
	const validate = (entries: unknown[]) => validateAttributionInventory({ schemaVersion: 1, entries });
	expect(validate(new Array(129))).toHaveLength(1);
	expect(validate([{ ...entry, evidence: Array(33).fill('LICENSE') }])).toHaveLength(1);
	expect(validate([{ ...entry, notice: 'a'.repeat(4097) }])).toHaveLength(1);
	expect(validate([entry])).toEqual([]);
});

it('withholds both attribution lists when inventory validation fails', async () => {
	vi.resetModules();
	vi.doMock('./attributions/contract.js', () => ({ validateAttributionInventory: () => ['invalid'] }));
	const { activeAttributions, plannedAttributions } = await import('./attributions/index.js');
	expect(activeAttributions).toEqual([]);
	expect(plannedAttributions).toEqual([]);
});

it('falls back to finite menu coordinates for invalid geometry', () => {
	expect(menuPosition(NaN, 90, 20, 20, 100, 100)).toEqual({ left: 0, top: 0 });
	expect(menuPosition(90, 90, -20, 20, 100, 100)).toEqual({ left: 0, top: 0 });
	expect(menuPosition(90, 90, 20, 20, 100, 100)).toEqual({ left: 80, top: 70 });
});

it.each(['invalid point', 'replaced list'])('refuses a pointer drop with %s', (failure) => {
	const node = Object.assign(new EventTarget(), {
		setPointerCapture: () => {}, hasPointerCapture: () => false
	});
	const hit = {};
	let list = { contains: () => true } as unknown as HTMLElement;
	vi.stubGlobal('window', Object.assign(new EventTarget(), { innerWidth: 200, innerHeight: 200 }));
	vi.stubGlobal('document', Object.assign(new EventTarget(), {
		documentElement: { dataset: {} }, body: { append: () => {} },
		createElement: () => ({ style: {}, offsetWidth: 10, offsetHeight: 10, remove: () => {} }),
		elementFromPoint: () => hit
	}));
	vi.stubGlobal('requestAnimationFrame', () => 1);
	vi.stubGlobal('cancelAnimationFrame', () => {});
	const drop = vi.fn();
	const drag = pointerQueueDrag(node as unknown as HTMLElement, {
		ids: () => ['track'], list: () => list, version: 1, hover: () => {}, drop, cancel: () => {}
	});
	const fire = (type: string, clientY: number) => node.dispatchEvent(
		Object.assign(new Event(type), { pointerType: 'touch', isPrimary: true, button: 0,
			pointerId: 1, clientX: 10, clientY })
	);
	try {
		fire('pointerdown', 10);
		fire('pointermove', 20);
		if (failure === 'replaced list') list = { ...list };
		fire('pointerup', failure === 'invalid point' ? Infinity : 20);
		expect(drop).not.toHaveBeenCalled();
	} finally { drag.destroy(); }
});

it('rejects a native track drop with nonfinite coordinates', () => {
	const setData = vi.fn<(type: string, token: string) => void>();
	const event = {
		clientX: 10, clientY: 10,
		dataTransfer: { types: ['application/x-nntune-track-ids'], setData,
			getData: () => setData.mock.calls[0]?.[1], setDragImage: () => {} }
	};
	vi.stubGlobal('window', new EventTarget());
	vi.stubGlobal('document', Object.assign(new EventTarget(), {
		documentElement: { dataset: {} }, body: { append: () => {} },
		createElement: () => ({ style: {}, remove: () => {} })
	}));
	startTrackDrag(event as unknown as DragEvent, ['track']);
	expect(droppedTrackDrag({ ...event, clientY: NaN } as unknown as DragEvent)).toBeNull();
	expect(droppedTrackDrag(event as unknown as DragEvent)?.ids).toEqual(['track']);
});

it('keeps mini-player hints isolated and clears invalid hints', () => {
	const hint = { trackId: 'track', title: 'Title', artist: null };
	setMiniPlayerHint(hint);
	hint.title = 'Changed';
	expect(readMiniPlayerHint()?.title).toBe('Title');
	setMiniPlayerHint({ ...hint, trackId: '' });
	expect(readMiniPlayerHint()).toBeNull();
});

it('closes the panel slot on malformed content', () => {
	setRightPanel((() => {}) as unknown as Parameters<typeof setRightPanel>[0]);
	setRightPanel('invalid snippet' as unknown as Parameters<typeof setRightPanel>[0]);
	expect(readRightPanel()).toBeNull();
});

it('bounds toast capacity and cancels the evicted timer', () => {
	vi.useFakeTimers();
	for (let i = 0; i < 9; i++) pushToast(String(i));
	expect(readToasts().map(({ message }) => message)).toEqual(['1', '2', '3', '4', '5', '6', '7', '8']);
	expect(vi.getTimerCount()).toBe(8);
});

it('replaces nonfinite toast durations with the default', () => {
	vi.useFakeTimers();
	pushToast('duration fallback', { durationMs: Infinity });
	vi.advanceTimersByTime(TOAST_DURATION_MS - 1);
	expect(readToasts().some(({ message }) => message === 'duration fallback')).toBe(true);
	vi.advanceTimersByTime(1);
	expect(readToasts()).toEqual([]);
});
