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
import { decodeBase64Url, encodeBase64Url, serializePublicKeyCredential, toCreationOptions, toRequestOptions } from './webauthn.js';

afterEach(() => {
	endTrackDrag();
	clearMiniPlayerHint();
	setRightPanel(null);
	clearToasts();
	vi.useRealTimers();
	vi.unstubAllGlobals();
	vi.doUnmock('./attributions/contract.js');
});

it.each([false, true])('refuses stale logout during marker setup (lock held: %s)', async (held) => {
	const values = new Map([[AUTH_GENERATION_KEY, 'original']]);
	const denied = () => { throw new DOMException('Unavailable', 'SecurityError'); };
	vi.stubGlobal('window', { localStorage: {
		setItem: held ? denied : (key: string, value: string) => values.set(key, value),
		getItem: (key: string) => values.get(key) ?? null,
		removeItem: held ? denied : (key: string) => values.delete(key)
	} });
	const switchAccount = () => values.set(AUTH_GENERATION_KEY, 'replacement');
	const clearCache = vi.fn();
	vi.stubGlobal('caches', { open: async () => switchAccount(), delete: clearCache });
	vi.stubGlobal('navigator', { locks: { request: async (_name: string, _opts: LockOptions, send: () => unknown) => {
		if (held) return send();
		switchAccount();
		denied();
	} } });
	const dispatch = vi.fn(async () => true);
	const stale = new Error('stale logout');
	await expect(runAuthMutation(new AbortController().signal, dispatch, { stale })).rejects.toBe(stale);
	expect(dispatch).not.toHaveBeenCalled();
	expect([...values]).toEqual([[AUTH_GENERATION_KEY, 'replacement']]);
	expect(clearCache).toHaveBeenCalledTimes(Number(held));
});

it('refuses a revocation if a newer auth attempt starts during admission', async () => {
	const gate = Promise.withResolvers<string[]>();
	const generation = 'original';
	vi.stubGlobal('window', { localStorage: { getItem: () => generation, setItem: () => {} } });
	vi.stubGlobal('navigator', { locks: { request: (
		_name: string, options: LockOptions, callback: (lock: object) => unknown
	) => options.ifAvailable ? callback({}) : Promise.reject(options.signal?.reason) } });
	vi.stubGlobal('caches', { keys: () => gate.promise });
	const dispatch = vi.fn(async () => true);
	const stale = new Error('stale revocation');
	const revocation = runAuthMutation(new AbortController().signal, dispatch, {
		stale, revocation: { generation }
	});
	const newer = AbortSignal.abort();
	await expect(runAuthMutation(newer, dispatch)).rejects.toBe(newer.reason);
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
		[offFirst, offSecond, offReplacement].forEach((off) => off());
	}
});

it.each(['_x', 'YQ==', 'YQ  ', '+w'])('rejects noncanonical base64url %j', (value) => {
	expect(() => decodeBase64Url(value)).toThrow();
});

const creation = { challenge: 'AQ', rp: { name: 'Example' },
	user: { id: 'AQ', name: 'example', displayName: 'Example' }, pubKeyCredParams: [] };
const bytesAsBase64 = (size: number) => encodeBase64Url(new Uint8Array(size));

it.each(['', 'A'.repeat(44), 'YQ='])('rejects an invalid challenge %j', (challenge) => {
	expect(() => toRequestOptions({ challenge })).toThrow();
	expect(() => toCreationOptions({ ...creation, challenge })).toThrow();
});

it('accepts padded challenges and fields at their size limits', () => {
	expect(toRequestOptions({ challenge: 'YQ==' }).challenge).toEqual(new Uint8Array([97]));
	expect(toRequestOptions({ challenge: bytesAsBase64(32), allowCredentials: Array(64).fill({ id: bytesAsBase64(1024) }) })
		.allowCredentials).toHaveLength(64);
	expect(toCreationOptions({ ...creation, user: { ...creation.user, id: bytesAsBase64(64) } }).user.id)
		.toEqual(new Uint8Array(64));
	expect(serialize(assertion, { id: bytesAsBase64(1024) }).id).toBe(bytesAsBase64(1024));
});

it.each(['', 'YQ==', bytesAsBase64(65)])('rejects an invalid registration handle %j', (id) => {
	expect(() => toCreationOptions({ ...creation, user: { ...creation.user, id } })).toThrow();
});

it.each([
	['empty id', [{ id: '' }]],
	['wrong type', [{ id: 'AQ', type: 'password' as PublicKeyCredentialType }]],
	['too many entries', Array(65).fill({ id: 'AQ' })],
	['sparse list', new Array(1)]
] as const)('rejects a descriptor list with %s', (_label, descriptors) => {
	expect(() => toRequestOptions({ challenge: 'AQ', allowCredentials: descriptors.slice() })).toThrow();
	expect(() => toCreationOptions({ ...creation, excludeCredentials: descriptors.slice() })).toThrow();
});

const buffer = new ArrayBuffer(1);
const assertion = { authenticatorData: buffer, signature: buffer, userHandle: null };
const serialize = (response: Record<string, ArrayBuffer | null>, fields: Record<string, unknown> = {}) =>
	serializePublicKeyCredential({ id: 'AQ', type: 'public-key', rawId: buffer, ...fields,
		response: { clientDataJSON: buffer, ...response }, getClientExtensionResults: () => ({})
	} as unknown as PublicKeyCredential);

it.each([
	['rawId', 1024], ['authenticatorData', 16_384], ['signature', 1024],
	['userHandle', 64], ['clientDataJSON', 16_384], ['attestationObject', 16_384]
] as const)('bounds the browser credential %s buffer', (field, limit) => {
	const atSize = (size: number) => field === 'rawId' ? serialize(assertion, { rawId: new ArrayBuffer(size) })
		: serialize({ ...(field === 'attestationObject' ? {} : assertion), [field]: new ArrayBuffer(size) });
	expect(() => atSize(0)).toThrow();
	expect(() => atSize(limit + 1)).toThrow();
	expect(atSize(limit).id).toBe('AQ');
});

it.each([{ type: 'password' }, { id: '' }, { id: '+' }, { id: bytesAsBase64(1025) }])('rejects malformed browser credential metadata %#', (fields) => {
	expect(() => serialize(assertion, fields)).toThrow();
});

it('bounds attribution entry counts, evidence lists and field lengths', () => {
	const entry = attributionInventory.entries[0];
	const validate = (entries: unknown[]) => validateAttributionInventory({ schemaVersion: 1, entries });
	expect(validate(new Array(129))).toHaveLength(1);
	expect(validate([{ ...entry, evidence: Array(33).fill('LICENSE') }])).toHaveLength(1);
	expect(validate([{ ...entry, notice: 'a'.repeat(4097) }])).toHaveLength(1);
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

function stubDragDom() {
	vi.stubGlobal('window', Object.assign(new EventTarget(), { innerWidth: 200, innerHeight: 200 }));
	vi.stubGlobal('document', Object.assign(new EventTarget(), {
		documentElement: { dataset: {} }, body: { append: () => {} },
		createElement: () => ({ style: {}, offsetWidth: 10, offsetHeight: 10, remove: () => {} }),
		elementFromPoint: () => ({})
	}));
}

it.each(['invalid point', 'replaced list'])('refuses a pointer drop with %s', (failure) => {
	const node = Object.assign(new EventTarget(), { setPointerCapture: () => {}, hasPointerCapture: () => false });
	let list = { contains: () => true } as unknown as HTMLElement;
	stubDragDom();
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
	stubDragDom();
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

it('refuses malformed panel content', () => {
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
