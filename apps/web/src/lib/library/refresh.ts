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

/** Largest library JSON body retained in memory (1 MiB, counted in UTF-8 bytes). */
export const LIBRARY_BODY_MAX_BYTES = 1_048_576;

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
 * driven by injected fetches in tests. Losing race branches (timeout wins or
 * a newer generation supersedes the request) cancel the in-flight response
 * body instead of abandoning it; the page mirrors `state` into its reactive
 * fields via `onStateChange`.
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
		let activeResponse: Response | null = null;
		let activeReader: ReadableStreamDefaultReader<Uint8Array> | null = null;
		let timeoutFired = false;

		function cancelResponse(resp: Response | null | undefined): void {
			if (!resp) return;
			try {
				const body = resp.body as ReadableStream | null;
				if (!body) return;
				const result = body.cancel();
				if (result instanceof Promise) result.catch(() => {});
			} catch {
				// A failing body cancel must never surface or corrupt state.
			}
		}

		function cancelReader(
			reader: ReadableStreamDefaultReader<Uint8Array> | null
		): void {
			if (!reader) return;
			try {
				const result = reader.cancel();
				if (result instanceof Promise) result.catch(() => {});
			} catch {
				// A failing reader cancel must never surface or corrupt state.
			}
		}

		function cancelActiveResponse(): void {
			// When `response.json()` (now `readJsonBody` below) holds the body
			// lock, `body.cancel()` rejects with "locked" and never reaches the
			// underlying source. Cancelling the active reader releases it.
			cancelReader(activeReader);
			if (activeResponse) cancelResponse(activeResponse);
		}

		async function readJsonBody(resp: Response): Promise<unknown> {
			const body = resp.body;
			if (!body) {
				// No readable stream to bound; fall back to a length-checked
				// read so an over-cap payload cannot bypass the cap via
				// `resp.json()`. Rejects through the same failure shape as a
				// timeout so the refresher falls back to unreachable/snapshot.
				const buffer = await resp.arrayBuffer();
				if (buffer.byteLength > LIBRARY_BODY_MAX_BYTES) {
					throw new Error('library request timed out');
				}
				return JSON.parse(new TextDecoder().decode(new Uint8Array(buffer)));
			}
			const reader = body.getReader() as ReadableStreamDefaultReader<Uint8Array>;
			activeReader = reader;
			try {
				const chunks: Uint8Array[] = [];
				let totalLength = 0;
				for (;;) {
					const { done, value } = await reader.read();
					if (done) break;
					if (value) {
						totalLength += value.length;
						if (totalLength > LIBRARY_BODY_MAX_BYTES) {
							// Stop at the cap: cancel the reader so the
							// producer stops and the trailing bytes are never
							// retained, then reject through the same failure
							// shape as a timeout (first load: unreachable;
							// after an authenticated success: snapshot
							// fallback). Chunk lengths are UTF-8 bytes.
							cancelReader(reader);
							throw new Error('library request timed out');
						}
						chunks.push(value);
					}
				}
				const combined = new Uint8Array(totalLength);
				let offset = 0;
				for (const chunk of chunks) {
					combined.set(chunk, offset);
					offset += chunk.length;
				}
				return JSON.parse(new TextDecoder().decode(combined));
			} finally {
				if (activeReader === reader) activeReader = null;
				try {
					reader.releaseLock();
				} catch {
					// The stream may already be closed or cancelled.
				}
			}
		}

		let abortReject!: (reason: Error) => void;
		const aborted = new Promise<never>((_, reject) => {
			abortReject = reject;
		});
		// Avoid an unhandled rejection when the abort fires after the races
		// already settled (the races themselves observe it when pending).
		aborted.catch(() => {});
		const onAbort = (): void => {
			cancelActiveResponse();
			abortReject(new Error('library request superseded'));
		};
		controller.signal.addEventListener('abort', onAbort, { once: true });

		const timeout = new Promise<never>((_, reject) => {
			timeoutId = setTimeout(() => {
				timeoutFired = true;
				cancelActiveResponse();
				controller.abort();
				reject(new Error('library request timed out'));
			}, timeoutMs);
		});

		try {
			const fetchPromise = fetchFn('/api/library', {
				credentials: 'include',
				signal: controller.signal
			});
			// A response that materializes after its race was already lost
			// (timeout won, or a newer generation aborted us) holds an open
			// stream the winner never reads. Release it instead of abandoning
			// it. Injected fetches deliberately ignore the abort signal, so
			// the signal alone cannot release a late-arriving body.
			void fetchPromise.then(
				(lateResponse) => {
					if (
						timeoutFired ||
						generation !== requestGeneration ||
						controller.signal.aborted
					) {
						cancelResponse(lateResponse);
					}
				},
				() => {}
			);
			const response = await Promise.race([fetchPromise, timeout, aborted]);
			if (generation !== requestGeneration || controller.signal.aborted) {
				cancelResponse(response);
				return;
			}
			activeResponse = response;
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
			const payload = await Promise.race([readJsonBody(response), timeout, aborted]);
			if (generation !== requestGeneration || controller.signal.aborted) {
				cancelResponse(response);
				return;
			}
			activeResponse = null;
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
			controller.signal.removeEventListener('abort', onAbort);
			activeResponse = null;
			activeReader = null;
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
