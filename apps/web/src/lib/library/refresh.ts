import { captureAuthFence } from '../auth-fence.js';
import { clearToasts } from '../shell/toasts.js';
import { validBinding, verifyLibrarySession } from './session-verification.js';
import {
	clearSnapshot,
	establishVerifiedLibraryScope,
	LIBRARY_AUTH_TRANSITION_KEY,
	libraryScopeBinding,
	loadSnapshot,
	saveSnapshot,
	toLibraryItems,
	type LibraryItem,
	type LibrarySnapshot,
	type SnapshotStorage
} from './cache.js';

export const LIBRARY_REQUEST_TIMEOUT_MS = 10_000;
export const LIBRARY_ACCESS_REFUSED_EVENT = 'nntune:library-access-refused';

/** Largest library JSON body retained in memory (1 MiB, counted in UTF-8 bytes). */
export const LIBRARY_BODY_MAX_BYTES = 1_048_576;

export interface LibraryRefreshState {
	items: LibraryItem[];
	staleAt: string | null;
	loading: boolean;
	loadError: string | null;
	libraryUnavailable: boolean;
	requiresSignIn: boolean;
	verificationSuspended?: boolean;
}

export interface LibraryRefreshUpdate {
	/** False after another request, an auth transition, or disposal. */
	isCurrent: () => boolean;
	/** A successful response or an explicit invalidation, never cached fallback. */
	authoritative: boolean;
	/** Library access is suspended or the scope was revoked. */
	authBoundary: boolean;
	sessionBinding?: string; // Verified label for the rows, not import provenance.
	deviceScope?: string; // Exact localStorage key eligible for the verified binding.
	/** Run collection dispatch/publication under the same receipt as the verified listing. */
	runIfCurrent?: <T>(work: () => T) => Promise<T | undefined>;
}

export interface LibraryRefresherDeps {
	/** Session-scoped snapshot storage, or null when storage is unavailable. */
	sessionStorage: () => SnapshotStorage | null;
	/** Clears every snapshot area and tells other tabs the session ended. */
	invalidateLibraryCache: () => void;
	/** Override the same-tab refusal signal when the route owns its local cleanup. */
	onAccessRefused?: () => void;
	/** Called after state mutations so the page can mirror them reactively. */
	onStateChange?: (state: LibraryRefreshState, update: LibraryRefreshUpdate) => void;
	timeoutMs?: number;
}

const legacyFixture = () => import.meta.env.MODE === 'test' || import.meta.env.MODE === 'browser-fixtures';

export interface LibraryRefresher {
	readonly state: LibraryRefreshState;
	refresh: (fetchFn?: typeof fetch) => Promise<void>;
	handleAuthTransition: (event: Pick<StorageEvent, 'key'>) => void;
	/** Same reset as a delivered transition, for same-window dispatch. */
	handleLocalAuthTransition: () => void;
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
		libraryUnavailable: false,
		requiresSignIn: false
	};
	let authenticatedThisLoad = false;
	let verified: { binding: string; fence: NonNullable<Awaited<ReturnType<typeof captureAuthFence>>> } | undefined;
	let inMemorySnapshot: LibrarySnapshot | null = null;
	let activeRequest: AbortController | null = null;
	let requestGeneration = 0;
	let disposed = false;

	function startReceipt(): () => boolean {
		const generation = ++requestGeneration;
		return () => !disposed && generation === requestGeneration;
	}

	async function refresh(fetchFn: typeof fetch = fetch): Promise<void> {
		if (disposed || activeRequest) return;
		const isCurrent = startReceipt();
		const controller = new AbortController();
		activeRequest = controller;
		state.loading = true;
		state.loadError = null;
		state.libraryUnavailable = false;
		state.requiresSignIn = false;
		let authoritative = false;
		let binding: string | null = null;
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
				if (typeof resp.arrayBuffer === 'function') {
					const buffer = await resp.arrayBuffer();
					if (buffer.byteLength > LIBRARY_BODY_MAX_BYTES) {
						throw new Error('library request timed out');
					}
					return JSON.parse(new TextDecoder().decode(new Uint8Array(buffer)));
				}
				// Runtimes (and injected test fetches) without `arrayBuffer`
				// cannot be measured before parsing; enforce the cap on the
				// serialized UTF-8 byte size instead so this path matches the
				// byte-counted streamed and arrayBuffer paths.
				const data = await resp.json();
				const serialized = JSON.stringify(data) ?? '';
				if (new TextEncoder().encode(serialized).length > LIBRARY_BODY_MAX_BYTES) {
					throw new Error('library request timed out');
				}
				return data;
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

		function runBeforeDeadline<T>(fence: NonNullable<typeof verified>['fence'], work: () => T) {
			const admitted = fence.run(() => isCurrent() && !controller.signal.aborted ? work() : undefined);
			return Promise.race([admitted, timeout, aborted]).catch(() => undefined);
		}

		function publish(sessionBinding?: string, deviceScope?: string): void {
			if (!isCurrent()) return;
			if (activeRequest === controller) activeRequest = null;
			state.loading = false;
			const fence = authenticatedThisLoad ? verified?.fence : undefined;
			deps.onStateChange?.(state, {
				isCurrent: () => isCurrent() && (!fence || fence.isCurrent()),
				authoritative, authBoundary: false, sessionBinding, deviceScope,
				runIfCurrent: authoritative && authenticatedThisLoad ? async (work) => {
					const commit = () => isCurrent() ? work() : undefined;
					return fence ? fence.run(commit) : commit(); // Headerless test fixtures only.
				} : undefined
			});
		}

		function restoreSnapshot(): void {
			const snapshot = authenticatedThisLoad ? inMemorySnapshot ?? loadSnapshot(deps.sessionStorage()) : null;
			if (!isCurrent()) return;
			state.items = snapshot?.items ?? [];
			state.staleAt = snapshot?.savedAt ?? null;
			if (!snapshot) state.loadError = 'Library is not reachable right now.';
		}

		function refuse(replaced = false, status?: number): true {
			if (isCurrent()) applyAuthTransition(replaced, true, status);
			return true;
		}

		try {
			deps.onStateChange?.(state, { isCurrent, authoritative: false, authBoundary: false });
			if (!isCurrent()) return;
			const capture = captureAuthFence();
			const fence = capture && await Promise.race([capture, timeout, aborted]);
			if (!isCurrent()) return;
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
						!isCurrent() ||
						controller.signal.aborted
					) {
						cancelResponse(lateResponse);
					}
				},
				() => {}
			);
			const response = await Promise.race([fetchPromise, timeout, aborted]);
			if (!isCurrent() || controller.signal.aborted) {
				cancelResponse(response);
				return;
			}
			activeResponse = response;
			if (!response.ok) {
				// Refusal bodies are never read. Detach before best-effort cleanup
				// so abort/disposal cannot cancel them again; never await cleanup.
				activeResponse = null;
				cancelResponse(response);
				if (!isCurrent()) return;
			}
			if (response.status === 404) {
				if (state.verificationSuspended) return void refuse();
				// The list endpoint has not landed yet. Do not treat a 404 as
				// session verification or expose a previously saved snapshot.
				authenticatedThisLoad = false;
				inMemorySnapshot = null;
				state.items = [];
				state.staleAt = null;
				state.libraryUnavailable = true;
				authoritative = true;
				return;
			}
			if (response.status === 401 || response.status === 403) {
				const commitRefusal = () => refuse(true, response.status);
				if (!(fence ? await runBeforeDeadline(fence, commitRefusal) : legacyFixture() && !capture && commitRefusal())) refuse();
				return;
			}
			if (!response.ok) throw new Error(`library request failed (${response.status})`);
			const label = (binding = response.headers?.get('x-nntune-session-binding') ?? null);
			const storage = deps.sessionStorage();
			if (!isCurrent()) return;
			const previous = verified?.binding ?? libraryScopeBinding(storage);
			if (!validBinding(label) && !(legacyFixture() && !previous && label === null)) return void refuse();
			if (previous && previous !== label) {
				if (fence && await runBeforeDeadline(fence, () => refuse(true))) return;
				return void refuse();
			}
			const payload = await Promise.race([readJsonBody(response), timeout, aborted]);
			if (!isCurrent() || controller.signal.aborted) {
				cancelResponse(response);
				return;
			}
			activeResponse = null;
			const fresh = toLibraryItems(payload);
			if (fresh === null) throw new Error('library response has an invalid shape');
			const commit = () => {
				const snapshot = saveSnapshot(storage, fresh, undefined, !label);
				if (!isCurrent()) return;
				authenticatedThisLoad = true;
				state.verificationSuspended = false;
				inMemorySnapshot = { ...snapshot, items: fresh };
				state.items = fresh;
				state.staleAt = null;
				authoritative = true;
			};
			if (!label) return commit();
			// Start verification under the lock, but never hold it over the wire.
			if (!fence) return void refuse();
			const started = await runBeforeDeadline(fence, () => {
				const result = Promise.race([verifyLibrarySession(label, controller.signal), timeout, aborted]);
				void result.catch(() => {}); // The lock request may resolve after the verifier.
				return { result };
			});
			if (!started) return void refuse();
			const matches = await started.result;
			const committed = await runBeforeDeadline(fence, () => {
				if (!matches) return refuse(true);
				const deviceScope = establishVerifiedLibraryScope(storage, label);
				verified = { binding: label, fence };
				commit();
				publish(label, deviceScope);
				return true;
			});
			if (!committed) refuse();
		} catch {
			if (!isCurrent()) return;
			if (state.verificationSuspended || binding !== null) return void refuse();
			if (authenticatedThisLoad && verified) {
				if (!await runBeforeDeadline(verified.fence, () => { restoreSnapshot(); publish(); return true; })) refuse();
			} else restoreSnapshot();
		} finally {
			if (timeoutId !== undefined) clearTimeout(timeoutId);
			controller.signal.removeEventListener('abort', onAbort);
			activeResponse = null;
			activeReader = null;
			if (activeRequest === controller) activeRequest = null;
			if (state.loading) publish();
		}
	}

	/**
	 * Shared local/delivered-transition reset: fence in-flight work (a late
	 * response can neither repopulate the view nor overwrite the replacement
	 * status), drop snapshots and memory, and enter the sign-in-required
	 * presentation. Never requests, rebroadcasts, navigates or retries.
	 */
	function applyAuthTransition(replaced = true, notify = false, status?: number): void {
		if (disposed) return;
		const isCurrent = startReceipt();
		if (!status) activeRequest?.abort();
		if (!isCurrent()) return;
		activeRequest = null;
		authenticatedThisLoad = false;
		if (replaced) verified = undefined;
		state.verificationSuspended = !replaced;
		inMemorySnapshot = null;
		if (replaced && notify) {
			clearToasts();
			if (deps.onAccessRefused) deps.onAccessRefused();
			else globalThis.window?.dispatchEvent?.(new Event(LIBRARY_ACCESS_REFUSED_EVENT));
		}
		if (status) deps.invalidateLibraryCache();
		else if (replaced) clearSnapshot(deps.sessionStorage());
		if (!isCurrent()) return;
		state.items = [];
		state.staleAt = null;
		state.requiresSignIn = replaced && status !== 403;
		state.loadError = status === 401 ? 'You are signed out or do not have access to this library.'
			: status === 403 ? 'You do not have access to this library.'
			: replaced ? 'Your session changed. Sign in again to see this library.'
			: 'Could not verify your library session. Try again.';
		state.loading = false;
		deps.onStateChange?.(state, { isCurrent, authoritative: true, authBoundary: true });
	}

	function handleAuthTransition(event: Pick<StorageEvent, 'key'>): void {
		if (event.key !== LIBRARY_AUTH_TRANSITION_KEY) return;
		applyAuthTransition();
	}

	function handleLocalAuthTransition(): void {
		applyAuthTransition();
	}

	function dispose(): void {
		disposed = true;
		activeRequest?.abort();
		activeRequest = null;
	}

	return { state, refresh, handleAuthTransition, handleLocalAuthTransition, dispose };
}
