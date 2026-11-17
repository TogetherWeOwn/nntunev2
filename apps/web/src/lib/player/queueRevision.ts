/** Route-owned authority for queue edits and explicit playback attempts. */
export function createQueueRevision() {
	let revision = 0;
	let alive = true;
	return {
		get revision() { return revision; },
		revoke() { return ++revision; },
		isCurrent(receipt: number) { return alive && receipt === revision; },
		destroy() { alive = false; revision += 1; }
	};
}
