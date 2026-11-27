import { MAX_OPAQUE_ID_CHARS } from './localStateBounds.js';
import { createResumeStore, type ResumeStoreDeps } from './resume.js';

type PositionObservation = 'sample' | 'flush' | 'seeked' | 'ended';

type PositionAttachment = {
	trackId: string;
	audio: HTMLAudioElement;
	/** The attachment's exact src attribute, as selected by the playback owner. */
	source: string;
	/** Checks the host's current attachment receipt, track and selected source. */
	ownsAttachment: () => boolean;
	storage: () => ResumeStoreDeps['storage'] | null;
};

/**
 * Opt-in Library position writer. The playback host remains responsible for native
 * events and attachment lifetime. Scope comes from accepted authenticated Library
 * data. The host must clear it before auth teardown or when response headers no
 * longer attest its current account, even if the replacement body is refused.
 */
export function createResumeController() {
	let scope: string | null = null;
	let current: object | undefined;

	return {
		setScope(value: string | null): void {
			const next = value?.length === 64 && /^[a-f0-9]+$/.test(value) ? value : null;
			if (next !== scope) current = undefined;
			scope = next;
		},
		attach({ trackId, audio, source, ownsAttachment, storage: getStorage }: PositionAttachment) {
			current = undefined;
			if (!scope || !source || !trackId || trackId.length > MAX_OPAQUE_ID_CHARS) return;
			const receipt = {};
			current = receipt;
			const storageKey = `nntune:library-position:v1:${scope}`;

			function matches(): boolean {
				const owns = () => current === receipt && ownsAttachment() && current === receipt;
				if (!owns() || !audio.isConnected || audio.getAttribute('src') !== source) return false;
				// Match the selected resource as well as the attribute. A decoder can
				// still report the previous resource during a src transition.
				const selected = audio.currentSrc;
				return selected !== '' && selected === audio.src && owns();
			}

			let observing = false;
			let lastPosition: number | undefined;
			let completion: 'pending' | 'cleared' | undefined;
			function observe(kind: PositionObservation): void {
				if (observing) return;
				observing = true;
				try {
					if (!matches() || audio.error !== null || audio.seeking) return;
					const ended = audio.ended;
					const clearing = ended && (kind === 'ended' || (kind === 'flush' && completion === 'pending'));
					if (clearing) {
						if (completion === 'cleared') return;
					} else if (ended || kind === 'ended' || (kind === 'flush' && completion)) {
						return;
					}
					const seconds = clearing ? null : audio.currentTime;
					if (seconds !== null) {
						if (!Number.isFinite(seconds) || seconds < 0 || seconds === lastPosition) return;
						if (seconds === 0 && lastPosition === undefined && (kind !== 'seeked' || completion)) return;
						if (kind === 'sample' && Math.abs(seconds - (lastPosition ?? 0)) < 5) return;
					}
					const ownsObservation = () => matches() && audio.error === null && !audio.seeking && audio.ended === ended;
					if (!ownsObservation()) return;
					if (clearing) {
						lastPosition = undefined;
						completion = 'pending';
					}
					const storage = getStorage();
					if (!storage || !ownsObservation()) return;
					// A failed or revoked read must not turn into an empty-map write.
					let read = false;
					let writeRequested = false;
					let saved = false;
					const store = createResumeStore({ storageKey, storage: {
						getItem(key) {
							const get = storage.getItem;
							if (!ownsObservation()) return null;
							const raw = get.call(storage, key);
							read = ownsObservation();
							return read ? raw : null;
						},
						setItem(key, value) {
							writeRequested = true;
							const set = storage.setItem;
							if (!read || !ownsObservation()) return;
							set.call(storage, key, value);
							saved = true;
						},
						removeItem(key) {
							const remove = storage.removeItem;
							if (read && ownsObservation()) remove.call(storage, key);
						}
					} });
					if (seconds === null) {
						store.clear(trackId);
						// An accepted read with no rewrite is an idempotent clear.
						if (read && (!writeRequested || saved)) completion = 'cleared';
					} else {
						store.save(trackId, seconds);
						if (saved) {
							lastPosition = seconds;
							completion = undefined;
						}
					}
				} catch {
					// Optional storage and native reads must stay outside transport settlement.
				} finally {
					observing = false;
				}
			}

			return {
				observe,
				/** Observe a final flush while still current, then retire the receipt. */
				detach() {
					observe('flush');
					if (current === receipt) current = undefined;
				}
			};
		}
	};
}
