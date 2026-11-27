type Events = HTMLElementEventMap & DocumentEventMap & WindowEventMap;

/** Keep listener registration and its matching teardown together. */
export function listen<K extends keyof Events>(
	target: EventTarget,
	type: K,
	handler: (event: Events[K]) => void,
	capture = false
): () => void {
	const listener = handler as EventListener;
	target.addEventListener(type, listener, capture);
	return () => target.removeEventListener(type, listener, capture);
}

export function stopListening(listeners: readonly (() => void)[]): void {
	const failures: unknown[] = [];
	for (const stop of listeners) {
		try { stop(); } catch (error) { failures.push(error); }
	}
	if (failures.length === 1) throw failures[0];
	if (failures.length > 1) throw new AggregateError(failures, 'Listener teardown failed');
}
