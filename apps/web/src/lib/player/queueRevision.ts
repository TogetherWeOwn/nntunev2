/** Route-owned authority for queue edits and explicit playback attempts. */
export function createQueueRevision() {
	let revision = 0;
	let alive = true;
	return {
		get revision() { return revision; },
		revoke() {
			if (revision === Number.MAX_SAFE_INTEGER) alive = false;
			if (alive) revision += 1;
			return revision;
		},
		isCurrent(receipt: number) { return alive && receipt === revision; },
		destroy() { alive = false; }
	};
}
