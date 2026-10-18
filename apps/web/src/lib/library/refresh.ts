import {
	clearSnapshot,
	LIBRARY_AUTH_TRANSITION_KEY,
	loadSnapshot,
	saveSnapshot,
	toLibraryItems,
	type LibraryItem,
	type LibrarySnapshot,
	type SnapshotStorage
} from './cache.js';

export const LIBRARY_REQUEST_TIMEOUT_MS = 10_000;

export interface LibraryRefreshState {
	items: LibraryItem[];
	staleAt: string | null;
	loading: boolean;
	loadError: string | null;
	libraryUnavailable: boolean;
}

export interface LibraryRefresherDeps {
	/** Session-scoped snapshot storage, or null when storage is unavailable. */
	sessionStorage: () => SnapshotStorage | null;
	/** Clears every snapshot area and tells other tabs the session ended. */
	invalidateLibraryCache: () => void;
	/** Called after state mutations so the page can mirror them reactively. */
	onStateChange?: (state: LibraryRefreshState) => void;
	timeoutMs?: number;
}

export interface LibraryRefresher {
	readonly state: LibraryRefreshState;
	refresh: (fetchFn?: typeof fetch) => Promise<void>;
	handleAuthTransition: (event: Pick<StorageEvent, 'key'>) => void;
	dispose: () => void;
}

/**
 * The library page's refresh state machine, extracted unchanged from
 * `routes/library/+page.svelte` so its fetch/timeout/generation races can be
 * driven by injected fetches in tests. Behavior (including which response
 * bodies the losing race branches leave behind) is preserved exactly; the
 * page mirrors `state` into its reactive fields via `onStateChange`.
 */
export function createLibraryRefresher(deps: LibraryRefresherDeps): LibraryRefresher {
	const timeoutMs = deps.timeoutMs ?? LIBRARY_REQUEST_TIMEOUT_MS;
	const state: LibraryRefreshState = {
		items: [],
		staleAt: null,
		loading: true,
		loadError: null,
		libraryUnavailable: false
	};
	let authenticatedThisLoad = false;
	let inMemorySnapshot: LibrarySnapshot | null = null;
	let activeRequest: AbortController | null = null;
	let requestGeneration = 0;

	function notify(): void {
		deps.onStateChange?.(state);
	}

	async function refresh(fetchFn: typeof fetch = fetch): Promise<void> {
		if (activeRequest) return;
		const generation = ++requestGeneration;
		state.loading = true;
		state.loadError = null;
		state.libraryUnavailable = false;
		notify();
		const controller = new AbortController();
		activeRequest = controller;
		let timeoutId: ReturnType<typeof setTimeout> | undefined;
		const timeout = new Promise<never>((_, reject) => {
			timeoutId = setTimeout(() => {
				controller.abort();
				reject(new Error('library request timed out'));
			}, timeoutMs);
		});

		try {
			const response = await Promise.race([
				fetchFn('/api/library', { credentials: 'include', signal: controller.signal }),
				timeout
			]);
			if (generation !== requestGeneration) return;
			if (response.status === 404) {
				// The list endpoint has not landed yet. Do not treat a 404 as
				// session verification or expose a previously saved snapshot.
				authenticatedThisLoad = false;
				inMemorySnapshot = null;
				state.items = [];
				state.staleAt = null;
				state.libraryUnavailable = true;
				return;
			}
			if (response.status === 401 || response.status === 403) {
				authenticatedThisLoad = false;
				inMemorySnapshot = null;
				deps.invalidateLibraryCache();
				state.items = [];
				state.staleAt = null;
				state.loadError = 'You are signed out or do not have access to this library.';
				return;
			}
			if (!response.ok) throw new Error(`library request failed (${response.status})`);
			const payload = await Promise.race([response.json(), timeout]);
			if (generation !== requestGeneration) return;
			const fresh = toLibraryItems(payload);
			if (fresh === null) throw new Error('library response has an invalid shape');
			authenticatedThisLoad = true;
			state.items = fresh;
			state.staleAt = null;
			const persistedSnapshot = saveSnapshot(deps.sessionStorage(), fresh);
			inMemorySnapshot = { ...persistedSnapshot, items: fresh };
		} catch {
			if (generation !== requestGeneration) return;
			// Do not trust a tab's snapshot until this page has verified its session.
			const snapshot = authenticatedThisLoad
				? (inMemorySnapshot ?? loadSnapshot(deps.sessionStorage()))
				: null;
			if (snapshot) {
				state.items = snapshot.items;
				state.staleAt = snapshot.savedAt;
			} else {
				state.items = [];
				state.staleAt = null;
				state.loadError = 'Library is not reachable right now.';
			}
		} finally {
			if (timeoutId !== undefined) clearTimeout(timeoutId);
			if (activeRequest === controller) {
				activeRequest = null;
				if (generation === requestGeneration) state.loading = false;
			}
			notify();
		}
	}

	function handleAuthTransition(event: Pick<StorageEvent, 'key'>): void {
		if (event.key !== LIBRARY_AUTH_TRANSITION_KEY) return;
		requestGeneration += 1;
		activeRequest?.abort();
		activeRequest = null;
		authenticatedThisLoad = false;
		inMemorySnapshot = null;
		clearSnapshot(deps.sessionStorage());
		state.items = [];
		state.staleAt = null;
		state.loadError = 'Your session changed. Sign in again to see this library.';
		state.loading = false;
		notify();
	}

	function dispose(): void {
		requestGeneration += 1;
		activeRequest?.abort();
	}

	return { state, refresh, handleAuthTransition, dispose };
}
