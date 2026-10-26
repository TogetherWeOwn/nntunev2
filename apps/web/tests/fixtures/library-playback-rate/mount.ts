import { mount, tick } from 'svelte';
import Library from '../../../src/routes/library/+page.svelte';
import { moveTrack, restoreQueue, selectTrack, type QueueState, type Track } from '../../../src/lib/player/queue.js';
import { createPlaybackSpeed } from '../../../src/lib/player/playbackSpeed.js';
import '../../../src/app.css';

/** Test-only bridge; the Vite pre-transform exports the route's existing setter. */
const library = mount(Library, { target: document.getElementById('app')! }) as unknown as {
	acceptanceReplaceQueue(state: QueueState): void;
};
let state: QueueState = { tracks: [], currentIndex: -1 };
async function replace(next: QueueState) {
	state = next;
	library.acceptanceReplaceQueue(next);
	await tick();
}

// No playback, speed, announcement or restore logic is invented by the
// harness. The queue bridge feeds the real page's existing replaceQueue;
// the helper bridge drives the REAL canonical playbackSpeed module with
// scripted fakes so the spec pins shipped helper regressions in-page.
type FakeStorage = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
type RateElement = { playbackRate: number };

const SPEED_KEY = 'nntune:playback-speed:v1';

function memoryStorage(seed: Record<string, string> = {}) {
	const backing = new Map<string, string>(Object.entries(seed));
	const storage: FakeStorage = {
		getItem: (key: string) => (backing.has(key) ? (backing.get(key) as string) : null),
		setItem: (key: string, value: string) => {
			backing.set(key, String(value));
		},
		removeItem: (key: string) => {
			backing.delete(key);
		}
	};
	return { storage, read: (key: string) => (backing.has(key) ? (backing.get(key) as string) : null) };
}

function deniedStorage(): FakeStorage {
	const deny = (): never => {
		throw new DOMException('Storage access blocked', 'SecurityError');
	};
	return { getItem: deny, setItem: deny, removeItem: deny };
}

function elementWith(rate = 1): RateElement {
	return { playbackRate: rate };
}

function clampedElement(limit: number, rate = 1): RateElement {
	let backing = rate;
	const el = {} as RateElement;
	Object.defineProperty(el, 'playbackRate', {
		configurable: true,
		enumerable: true,
		get: () => backing,
		set: (value: number) => {
			backing = Math.min(Number(value), limit);
		}
	});
	return el;
}

function throwingSetterElement(rate = 1): RateElement {
	const el = {} as RateElement;
	Object.defineProperty(el, 'playbackRate', {
		configurable: true,
		enumerable: true,
		get: () => rate,
		set: () => {
			throw new Error('rate setter unavailable');
		}
	});
	return el;
}

function throwingGetterElement(): RateElement {
	const el = {} as RateElement;
	Object.defineProperty(el, 'playbackRate', {
		configurable: true,
		enumerable: true,
		get: (): number => {
			throw new Error('rate getter unavailable');
		},
		set: (_value: number) => {
			// The write lands; only the readback is broken.
		}
	});
	return el;
}

function nonFiniteGetterElement(): RateElement {
	const el = {} as RateElement;
	Object.defineProperty(el, 'playbackRate', {
		configurable: true,
		enumerable: true,
		get: () => NaN,
		set: (_value: number) => {
			// The write lands; the readback is non-finite.
		}
	});
	return el;
}

type Scenario = () => unknown;

const scenarios: Record<string, Scenario> = {
	'recall-valid': () => {
		const memory = memoryStorage({ [SPEED_KEY]: '1.3' });
		const el = elementWith(1);
		const speed = createPlaybackSpeed({ storage: memory.storage, audio: () => el });
		const before = { current: speed.current(), element: el.playbackRate, stored: memory.read(SPEED_KEY) };
		const applied = speed.apply();
		return {
			ok: true,
			before,
			applied,
			current: speed.current(),
			element: el.playbackRate,
			stored: memory.read(SPEED_KEY)
		};
	},
	'recall-shapes': () => {
		const seeds: Array<[string, string | null]> = [
			['valid', '1.5'],
			['corrupt', 'garbage'],
			['blank', ''],
			['spaces', '   '],
			['nan', 'NaN'],
			['infinite', 'Infinity'],
			['high', '9'],
			['low', '-3'],
			['missing', null]
		];
		const currents: Record<string, number> = {};
		for (const [name, seed] of seeds) {
			const memory = memoryStorage(seed === null ? {} : { [SPEED_KEY]: seed });
			currents[name] = createPlaybackSpeed({ storage: memory.storage, audio: () => null }).current();
		}
		return { ok: true, currents };
	},
	'storage-denied': () => {
		const el = elementWith(1);
		const speed = createPlaybackSpeed({ storage: deniedStorage(), audio: () => el });
		const initial = speed.current();
		const set = speed.set(1.5);
		const applied = speed.apply();
		return { ok: true, initial, set, applied, current: speed.current(), element: el.playbackRate };
	},
	'no-element': () => {
		const memory = memoryStorage();
		const speed = createPlaybackSpeed({ storage: memory.storage, audio: () => null });
		const set = speed.set(1.5);
		const applied = speed.apply();
		return { ok: true, set, applied, current: speed.current(), stored: memory.read(SPEED_KEY) };
	},
	'audio-throws': () => {
		const memory = memoryStorage({ [SPEED_KEY]: '1.5' });
		const audio = (): RateElement => {
			throw new Error('no element');
		};
		const speed = createPlaybackSpeed({ storage: memory.storage, audio });
		return {
			ok: true,
			current: speed.current(),
			set: speed.set(1.25),
			apply: speed.apply(),
			stored: memory.read(SPEED_KEY)
		};
	},
	'clamp-adopt': () => {
		const memory = memoryStorage();
		const el = clampedElement(1.25, 1);
		const speed = createPlaybackSpeed({ storage: memory.storage, audio: () => el });
		const set = speed.set(1.5);
		return {
			ok: true,
			requested: 1.5,
			set,
			current: speed.current(),
			element: el.playbackRate,
			stored: memory.read(SPEED_KEY)
		};
	},
	'setter-throws': () => {
		const memory = memoryStorage({ [SPEED_KEY]: '1' });
		const el = throwingSetterElement(1);
		const speed = createPlaybackSpeed({ storage: memory.storage, audio: () => el });
		const set = speed.set(1.5);
		return { ok: true, set, current: speed.current(), element: el.playbackRate, stored: memory.read(SPEED_KEY) };
	},
	'getter-throws': () => {
		const memory = memoryStorage({ [SPEED_KEY]: '1' });
		const el = throwingGetterElement();
		const speed = createPlaybackSpeed({ storage: memory.storage, audio: () => el });
		const set = speed.set(1.5);
		return { ok: true, set, current: speed.current(), stored: memory.read(SPEED_KEY) };
	},
	'getter-nonfinite': () => {
		const memory = memoryStorage({ [SPEED_KEY]: '1' });
		const el = nonFiniteGetterElement();
		const speed = createPlaybackSpeed({ storage: memory.storage, audio: () => el });
		const set = speed.set(1.5);
		return { ok: true, set, current: speed.current(), stored: memory.read(SPEED_KEY) };
	},
	'adopt-native': () => {
		const memory = memoryStorage();
		const el = elementWith(1);
		const speed = createPlaybackSpeed({ storage: memory.storage, audio: () => el });
		const adopted = speed.adoptNativeRate(1.3);
		const storedAfter = memory.read(SPEED_KEY);
		const kept = speed.adoptNativeRate(NaN);
		const storedKept = memory.read(SPEED_KEY);
		const clamped = speed.adoptNativeRate(9);
		return {
			ok: true,
			adopted,
			storedAfter,
			kept,
			storedKept,
			clamped,
			current: speed.current(),
			storedFinal: memory.read(SPEED_KEY),
			element: el.playbackRate
		};
	},
	'set-nonfinite': () => {
		const memory = memoryStorage();
		const el = elementWith(1);
		const speed = createPlaybackSpeed({ storage: memory.storage, audio: () => el });
		const set = speed.set(NaN);
		return { ok: true, set, current: speed.current(), element: el.playbackRate, stored: memory.read(SPEED_KEY) };
	}
};

function runHelperScenario(name: string): unknown {
	const run = scenarios[name];
	if (!run) return { ok: false, error: `unknown scenario: ${name}` };
	try {
		return run();
	} catch (error) {
		return { ok: false, error: error instanceof Error ? error.message : String(error) };
	}
}

declare global {
	interface Window {
		libraryPlaybackRate: {
			ready: boolean;
			replace(state: QueueState): Promise<void>;
			select(index: number): Promise<void>;
			move(from: number, to: number): Promise<void>;
			restore(raw: string, catalogue: Track[]): Promise<void>;
			helper(scenario: string): unknown;
		};
	}
}

window.libraryPlaybackRate = {
	ready: true,
	replace,
	select: (index) => replace(selectTrack(state, index)),
	move: (from, to) => replace(moveTrack(state, from, to)),
	restore: (raw, catalogue) => replace(restoreQueue(raw, catalogue)),
	helper: runHelperScenario
};
