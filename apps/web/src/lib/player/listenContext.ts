export interface ListenContext {
	/** Snapshot synchronously at playback start; never await a context for an event. */
	current(): string | undefined;
	/** A session transition or teardown permanently invalidates this scope. */
	drop(): void;
}

/** Open once at player-page mount, not on play, retry, or media-element replacement. */
export function openListenContext(baseUrl: string, fetchFn: typeof fetch = fetch): ListenContext {
	const controller = new AbortController();
	let context: string | undefined;

	async function load(): Promise<void> {
		try {
			const signal = AbortSignal.any([controller.signal, AbortSignal.timeout(10_000)]);
			const response = await fetchFn(`${baseUrl.replace(/\/+$/, '')}/v1/plays/events/context`, {
				method: 'POST',
				credentials: 'include',
				cache: 'no-store',
				redirect: 'error',
				headers: { accept: 'application/json' },
				signal
			});
			if (!response.ok) return;
			const body: { context?: unknown } | null = await response.json();
			const value = body?.context;
			if (!signal.aborted && typeof value === 'string' && /^[\w-]{43}$/.test(value)) {
				context = value;
			}
		} catch {
			// Missing context disables history, not playback.
		}
	}
	void load();

	return {
		current: () => context,
		drop() {
			context = undefined;
			controller.abort();
		}
	};
}
