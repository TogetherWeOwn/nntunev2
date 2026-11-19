import { createResumeStore, type ResumeStoreDeps } from './resume.js';

type StorageReader = () => ResumeStoreDeps['storage'] | null;

/** Library position persistence. This owner never restores a position. */
export function createResumeController() {
	let account: { scope: string } | undefined;

	return {
		setScope(scope: string | null): void {
			if (scope !== account?.scope) account = scope ? { scope } : undefined;
		},
		capture(trackId: string, getStorage: StorageReader, ownsSource: () => boolean) {
			const receipt = account;
			if (!receipt) return;
			const storageKey = `nntune:library-position:v1:${receipt.scope}`;
			function write(seconds: number | null, matches: () => boolean): void {
				const isCurrent = () => account === receipt && ownsSource() && matches() &&
					account === receipt && ownsSource();
				if (!isCurrent()) return;
				const storage = getStorage();
				if (!storage || !isCurrent()) return;
				// The store reads before writing. Fence both calls so a reentrant
				// storage read cannot publish after the account or source retires.
				const store = createResumeStore({ storageKey, storage: {
					getItem(key) {
						const get = storage.getItem;
						return isCurrent() ? get.call(storage, key) : null;
					},
					setItem(key, value) {
						const set = storage.setItem;
						if (isCurrent()) set.call(storage, key, value);
					},
					removeItem(key) { if (isCurrent()) storage.removeItem(key); }
				} });
				if (seconds === null) store.clear(trackId);
				else store.save(trackId, seconds);
			}
			return write;
		}
	};
}
