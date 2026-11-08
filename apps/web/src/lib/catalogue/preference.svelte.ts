import { ApiClient, type ApiClientOptions } from '../api/client.js';
import { catalogueAuthGeneration } from './auth.svelte.js';
import { collectionPreference, type PreferenceFailure, type PreferenceKind } from './preferences.js';

/** One route-owned preference; old requests cannot publish into a new target or account. */
export function createCollectionPreference(kind: PreferenceKind, options: ApiClientOptions) {
	let value = $state<boolean | null>(null);
	let pending = $state(false);
	let failure = $state<PreferenceFailure | null>(null);
	let target: { mbid: string; generation: number; controller: AbortController } | null = null;
	let request = 0;

	function clear() {
		request += 1;
		target?.controller.abort();
		target = null;
		value = null;
		pending = false;
		failure = null;
	}

	async function refresh(enabled?: boolean) {
		const current = target;
		if (!current || pending || current.generation !== catalogueAuthGeneration()) return;
		const sequence = ++request;
		pending = true;
		failure = null;
		const client = new ApiClient({ ...options, signal: current.controller.signal });
		const result = await collectionPreference(client, kind, current.mbid, enabled);
		if (current !== target || sequence !== request || current.generation !== catalogueAuthGeneration()) return;
		pending = false;
		if (result.failure) {
			// A failed write may have landed. Keep the last confirmed value but
			// require a fresh GET before enabling another mutation.
			failure = result.failure;
		} else {
			value = result.value;
		}
	}

	return {
		get value() { return value; },
		get pending() { return pending; },
		get failure() { return failure; },
		load(mbid: string, generation: number) {
			clear();
			target = { mbid, generation, controller: new AbortController() };
			return refresh();
		},
		clear,
		retry() { return refresh(); },
		toggle() {
			if (value === null || failure) return;
			return refresh(!value);
		}
	};
}
