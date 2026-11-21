import { get, writable } from 'svelte/store';
import { displayTitle, shortTitle } from '../library/order.js';
import { resolveStreamUrl } from '../library/stream-url.js';
import { currentTrack, naturalEndPolicy, selectTrack, type QueueState, type RepeatMode } from './queue.js';
import type { createQueueController, QueuedPlaySource } from './queueController.js';
import { describeJump, jumpMedia, type JumpDirection } from './relativeJump.js';
import { createPendingShareSeek } from './pendingShareSeek.js';
import { restartIsAvailable } from './restartAvailability.js';
import { bindSystemTransport } from './systemTransport.js';
import type { ShareTarget } from './shareTimestamp.js';

export type MediaAttachment = { readonly audio: HTMLAudioElement; playing: boolean };
export type PlaybackStatus = 'unavailable' | 'paused' | 'starting' | 'playing' | 'blocked' | 'error' | 'retrying';

type PlayOutcome = 'playing' | 'blocked' | 'error' | undefined;
type QueueAuthority = Pick<ReturnType<typeof createQueueController>,
	'snapshot' | 'subscribe' | 'revision' | 'revoke' | 'isCurrent' | 'purging'>;

export type MediaSelection = {
	commit: (queue: QueueState) => boolean;
	flush: () => Promise<void>;
	browseCopy: { invalidate: () => void };
	announce: (message: string, replay?: boolean) => void;
};

const REPEAT_KEY = 'nntune:library-repeat:v1';

function describeRestart(words: string, title: string): string {
	const suffix = ` ${title}.`;
	if (words.length + suffix.length <= 99) return `${words}${suffix}`;
	let end = 97 - words.length;
	// Never cut between the halves of a surrogate pair.
	const last = title.charCodeAt(end - 1);
	if (last >= 0xd800 && last <= 0xdbff) end -= 1;
	return `${words} ${title.slice(0, end)}…`;
}

// Queued native events belong to the element, not to a receipt or controller.
const mountedAudio = new WeakSet<HTMLAudioElement>();
const nativeIsConnected = typeof Node === 'undefined' ? undefined : Object.getOwnPropertyDescriptor(Node.prototype, 'isConnected')?.get;
const nativeGetAttribute = typeof Element === 'undefined' ? undefined : Element.prototype.getAttribute;

function matchesNativeSource(audio: HTMLAudioElement, source: string): boolean {
	return nativeIsConnected?.call(audio) === true && nativeGetAttribute?.call(audio, 'src') === source;
}

export function createMediaController(queue: QueueAuthority, selection?: MediaSelection, matchesNative = matchesNativeSource) {
	let current: (MediaAttachment & { listeners: AbortController }) | undefined;
	let attachmentVersion = 0;
	let endGeneration = 0;
	let playIntent = 0;
	let pendingPlay: { result: Promise<PlayOutcome>; finish: (outcome: PlayOutcome) => void } | undefined;
	const playbackStatus = writable<PlaybackStatus>('unavailable');
	const repeat = writable<RepeatMode>('off');
	const seekFeedback = writable({ restartCanSeek: false, restartStatus: '', restartStatusVersion: 0,
		jumpStatus: '', jumpStatusVersion: 0 });
	let lastJumpTarget: number | undefined;
	let destroyed = false;
	let positionIntent = 0;
	const pendingShare = createPendingShareSeek();
	let applyingShare: { revision: number; retry: boolean } | undefined;
	let positioning = false;
	let nextPosition: (() => void) | undefined;

	function pause(audio: HTMLAudioElement | undefined): void {
		if (audio && !audio.paused) audio.pause();
	}

	const selectedSource = () => resolveStreamUrl(currentTrack(queue.snapshot));

	function settlePlay(outcome?: PlayOutcome): void {
		pendingPlay?.finish(outcome);
		pendingPlay = undefined;
	}

	function invalidatePlay(): void {
		++playIntent;
		settlePlay();
	}

	function readPlayback(
		audio: HTMLAudioElement, matches: () => boolean, active: PlaybackStatus, stopped: PlaybackStatus = 'paused'
	): PlaybackStatus | undefined {
		const state = audio.error ? 'error' : audio.paused || audio.ended ? stopped : active;
		if (!matches()) return;
		playbackStatus.set(state);
		return state;
	}

	function transportTarget() {
		const attachment = current;
		const source = selectedSource();
		if (!attachment || !source) return;
		const { audio } = attachment;
		const matches = (ownsIntent = () => true) => matchesSource(audio, source, () =>
			current === attachment && !destroyed && !queue.purging && selectedSource() === source && ownsIntent());
		return { audio, attachment, matches };
	}

	async function startCurrent(retry: boolean): Promise<void> {
		const target = transportTarget();
		if (!target || pendingPlay) return;
		const { audio, matches } = target;
		const trackId = retry ? currentTrack(queue.snapshot)?.id : undefined;
		const sameTrack = () => !retry || currentTrack(queue.snapshot)?.id === trackId;
		const previousIntent = playIntent;
		if (!matches() || (retry ? get(playbackStatus) !== 'error' || !audio.error
			: audio.error || (!audio.paused && !audio.ended)) ||
			!matches(() => playIntent === previousIntent && sameTrack())) return;
		++playIntent;
		if (retry) ++endGeneration;
		let finish!: (outcome: PlayOutcome) => void;
		const result = new Promise<PlayOutcome>((resolve) => { finish = resolve; });
		const attempt = pendingPlay = { result, finish };
		const matchesAttempt = () => matches(() => pendingPlay === attempt && sameTrack());
		let state: PlaybackStatus | undefined;
		try {
			playbackStatus.set(retry ? 'retrying' : 'starting');
			if (retry) {
				const load = audio.load;
				const stillFailed = audio.error !== null;
				if (!matchesAttempt()) return;
				if (!stillFailed) {
					readPlayback(audio, matchesAttempt, 'starting');
					return;
				}
				load.call(audio);
				if (!matchesAttempt()) return;
			}
			// Stay in the gesture, on the same decoder. Only explicit Retry can reload it.
			const play = audio.play;
			if (!matchesAttempt()) return;
			await play.call(audio);
			if (matchesAttempt()) state = readPlayback(audio, matchesAttempt, 'playing');
		} catch (error) {
			if (!matchesAttempt()) return;
			// The queued native pause event may already see a resumed decoder.
			const interrupted = error instanceof DOMException && error.name === 'AbortError' && !audio.error;
			let matchesOutcome = matchesAttempt;
			if (interrupted) {
				if (retry && !matchesAttempt()) return;
				invalidatePlay();
				const intent = playIntent;
				matchesOutcome = retry ? () => matches(() => playIntent === intent && sameTrack()) : matches;
			}
			state = readPlayback(audio, matchesOutcome, 'starting', interrupted ? 'paused' : 'blocked');
		} finally {
			if (pendingPlay === attempt) settlePlay(state === 'playing' || state === 'blocked' || state === 'error' ? state : undefined);
		}
	}

	function playCurrent(): Promise<void> {
		return startCurrent(false);
	}

	function pauseCurrent(): void {
		const target = transportTarget();
		if (!target || !target.matches()) return;
		const { audio, attachment, matches } = target;
		invalidatePlay();
		endGeneration += 1;
		const intent = playIntent;
		const ownsPause = () => matches(() => playIntent === intent);
		pause(audio);
		if (!ownsPause()) return;
		if (audio.paused || audio.ended) attachment.playing = false;
		readPlayback(audio, ownsPause, get(playbackStatus));
	}

	function clearJumpFeedback(): void {
		const feedback = get(seekFeedback);
		if (feedback.jumpStatus) seekFeedback.set({ ...feedback, jumpStatus: '' });
	}

	function clearSeekFeedback(): void {
		lastJumpTarget = undefined;
		seekFeedback.update((feedback) => ({ ...feedback, restartCanSeek: false, restartStatus: '', jumpStatus: '' }));
	}

	function observeSeeking(audio: HTMLAudioElement, matches: () => boolean): void {
		if (!matches()) return;
		const target = lastJumpTarget;
		// Keep the jump's own queued event, even after a newer no-write outcome.
		if (target !== undefined) {
			try {
				if (Math.abs(audio.currentTime - target) <= 0.05) return;
			} catch { /* An unreadable position cannot confirm the jump target. */ }
		}
		if (!matches() || lastJumpTarget !== target) return;
		lastJumpTarget = undefined;
		clearJumpFeedback();
	}

	function retire(): HTMLAudioElement | undefined {
		const previous = current;
		current = undefined;
		// Retire the native receipt without imposing replacement's share-clear policy on remove/clear.
		attachmentVersion += 1;
		previous?.listeners.abort();
		invalidatePlay();
		playbackStatus.set('unavailable');
		return previous?.audio;
	}

	function reset(): void {
		pause(retire());
	}

	function purgeAtAuthBoundary(clearHint: () => void): void {
		pendingShare.clear();
		positionIntent += 1;
		nextPosition = undefined;
		const audio = retire();
		try {
			clearHint();
			clearSeekFeedback();
		} finally {
			try { pause(audio); } catch { /* The retired element cannot own player state. */ }
		}
	}

	async function playSelected(
		base: QueueState, index: number, source: QueuedPlaySource, suffix = ''
	): Promise<void> {
		if (!selection || queue.purging) return;
		const target = base.tracks[index];
		if (!target || destroyed || !source.isCurrent()) return;
		const { streamUrl } = source;
		const tracks = [...base.tracks];
		tracks[index] = { ...target, streamUrl };
		if (!selection.commit(selectTrack({ ...base, tracks }, index)) || !source.isCurrent() || destroyed) return;

		const receipt = queue.revoke();
		let attachment = current;
		let version = attachmentVersion;
		const intent = playIntent;
		const ownsQueue = () => !destroyed && source.isCurrent() && queue.isCurrent(receipt);
		const isCurrent = () => ownsQueue() && current === attachment && attachmentVersion === version;
		const ownsPlay = () => isCurrent() && playIntent === intent;
		const title = Array.from(target.title.trim() || target.id).slice(0, 60).join('');
		let lead = `Selected ${title}; no stream to play.`;
		if (streamUrl) {
			// A same-attachment gesture shares the compact outcome, even if it settles during flush.
			const sharedPlay = pendingPlay?.result;
			await selection.flush();
			// A source change retires the old node before commit. In that case
			// flush can admit one attachment, but not a reset or replacement while waiting.
			if (!ownsQueue() || attachmentVersion !== version + Number(!attachment && !!current)) return;
			attachment = current;
			version = attachmentVersion;
			const audio = attachment?.audio;
			lead = `Selected ${title}, paused: playback was blocked.`;
			if (sharedPlay && audio) {
				const outcome = await sharedPlay;
				if (!matchesSource(audio, streamUrl, isCurrent)) return;
				if (outcome === 'error' || audio.error) lead = `Selected ${title}; audio unavailable.`;
				else if (outcome !== 'blocked') lead = outcome && !audio.paused && !audio.ended ? `Playing ${title}.` : '';
				// An interrupted Play can still own the committed queue result.
				if (!isCurrent()) return;
				const message = [playIntent === intent && lead, suffix].filter(Boolean).join(' ');
				if (message) selection.announce(message, true);
				return;
			}
			if (!ownsPlay()) return;
			try {
				if (audio?.isConnected && audio.getAttribute('src') === streamUrl && ownsPlay()) {
					const play = audio.play;
					if (!ownsPlay()) return;
					await play.call(audio);
					lead = `Playing ${title}.`;
				}
			} catch {
				// No retry and no pause or selection change on a retired receipt.
				if (ownsPlay() && attachment) attachment.playing = false;
			}
			if (ownsPlay() && audio?.error) lead = `Selected ${title}; audio unavailable.`;
		}
		if (ownsPlay()) selection.announce(suffix ? `${lead} ${suffix}` : lead, true);
	}

	function matchesSource(audio: HTMLAudioElement, source: string, isCurrent: () => boolean): boolean {
		try {
			// Either instance accessor can change what the other last observed. End
			// with a DOM snapshot that does not call back into those accessors.
			return isCurrent() && audio.isConnected && isCurrent() &&
				audio.getAttribute('src') === source && isCurrent() && audio.isConnected && isCurrent() &&
				matchesNative(audio, source);
		} catch {
			return false;
		}
	}

	function runPosition(action: () => void): void {
		// A setter can call a newer gesture before storing its own value. Revoke
		// the old intent immediately, but let the native call unwind before writing again.
		nextPosition = action;
		if (positioning) return;
		positioning = true;
		try {
			while (nextPosition) {
				const run = nextPosition;
				nextPosition = undefined;
				run();
			}
		} finally {
			positioning = false;
			nextPosition = undefined;
		}
	}

	function retryShare(audio: HTMLAudioElement | undefined, revision: number): void {
		if (!audio || !pendingShare.current || pendingShare.revision !== revision || nextPosition) return;
		if (applyingShare?.revision === revision) {
			applyingShare.retry = true;
			return;
		}
		runPosition(() => {
			const attempt = { revision, retry: false };
			applyingShare = attempt;
			try {
				pendingShare.apply(audio, revision, () => attempt.retry);
				// Keep the receipt for at most one follow-up; newer positions go first.
				if (attempt.retry && !nextPosition) pendingShare.apply(audio, revision);
			} finally {
				applyingShare = undefined;
			}
		});
	}

	function captureShareSeek(intent: number, { trackId, seconds }: ShareTarget, routeIsCurrent: () => boolean) {
		if (!routeIsCurrent() || destroyed || intent !== positionIntent) return;
		const track = currentTrack(queue.snapshot);
		if (track?.id !== trackId) return;
		const streamUrl = resolveStreamUrl(track);
		if (!streamUrl) return;
		// Before the keyed node mounts, only the first upcoming attachment can inherit this seek.
		const version = current ? attachmentVersion : attachmentVersion + 1;
		const revision = pendingShare.set({ trackId, streamUrl, seconds }, (audio) => ({
			isCurrent: () => matchesSource(audio, streamUrl, () => {
				if (!routeIsCurrent() || destroyed || current?.audio !== audio ||
					attachmentVersion !== version || positionIntent !== intent) return false;
				// Other queue edits need not revoke a seek for this track and source.
				const selected = currentTrack(queue.snapshot);
				return selected?.id === trackId && resolveStreamUrl(selected) === streamUrl;
			})
		}));
		clearJumpFeedback();
		return () => retryShare(current?.audio, revision);
	}

	function jump(direction: JumpDirection): void {
		if (destroyed || queue.purging) return;
		const track = currentTrack(queue.snapshot);
		const source = resolveStreamUrl(track);
		const attachment = current;
		const audio = attachment?.audio;
		const version = attachmentVersion;
		const queueRevision = queue.revision;
		const intent = ++positionIntent;
		// Cancel on acceptance, even if the native seek is deferred or refused.
		if (track) pendingShare.supersede(track.id, source);
		const revision = pendingShare.revision;
		const isCurrent = () => !destroyed && attachmentVersion === version && queue.isCurrent(queueRevision) &&
			positionIntent === intent && pendingShare.revision === revision;
		runPosition(() => {
			const matches = () => !!audio && !!source && matchesSource(audio, source, isCurrent);
			const outcome = audio && matches() ? jumpMedia(audio, direction, matches, () => {
				if (current === attachment) clearJumpFeedback();
			}) : undefined;
			if (outcome ? !matches() : !isCurrent()) return;
			if (outcome?.kind === 'jumped') lastJumpTarget = outcome.target;
			seekFeedback.update((feedback) => ({
				...feedback, jumpStatus: describeJump(direction, outcome ?? { kind: 'unavailable' }),
				jumpStatusVersion: feedback.jumpStatusVersion + 1
			}));
		});
	}

	function restart(): void {
		const track = currentTrack(queue.snapshot);
		const source = resolveStreamUrl(track);
		if (destroyed || !track || !source) return;
		const title = track.title?.trim() ? track.title : track.id;
		const attachment = current;
		const queueRevision = queue.revision;
		const intent = ++positionIntent;
		// Supersede before any native read, including an unavailable or refused seek.
		pendingShare.supersede(track.id, source);
		if (!attachment) return;
		const { audio } = attachment;
		const revision = pendingShare.revision;
		const isCurrent = () => queue.isCurrent(queueRevision) && current === attachment &&
			positionIntent === intent && pendingShare.revision === revision;
		const matches = () => matchesSource(audio, source, isCurrent);
		runPosition(() => {
			if (!matches()) return;

			let words = 'Could not restart';
			let available = false;
			try {
				available = restartIsAvailable(audio, matches);
				if (available && matches()) {
					const position = audio.currentTime;
					if (Number.isFinite(position) && matches()) {
						const atStart = Math.abs(position) <= 0.05;
						if (atStart) words = 'Already at the start of';
						audio.currentTime = 0;
						// Queue edits can revoke readback without retiring the old position feedback.
						if (current === attachment) clearJumpFeedback();
						if (!atStart) {
							if (!matches()) return;
							const readback = audio.currentTime;
							if (Number.isFinite(readback) && Math.abs(readback) <= 0.05) words = 'Restarted';
						}
					}
				}
			} catch {
				// Unreadable positions and refused assignments cannot prove a restart.
			}
			if (matches()) seekFeedback.update((feedback) => ({
				...feedback, restartCanSeek: available && feedback.restartCanSeek,
				restartStatus: describeRestart(words, title), restartStatusVersion: feedback.restartStatusVersion + 1
			}));
		});
	}

	async function handleEnded(attachment: MediaAttachment, source: string, matchesOrigin: () => boolean): Promise<void> {
		if (attachment.audio.ended && matchesOrigin()) {
			invalidatePlay();
			readPlayback(attachment.audio, matchesOrigin, 'playing');
		}
		if (!selection) return;
		const { commit, flush, browseCopy, announce } = selection;
		const { audio } = attachment;
		const intent = positionIntent;
		const version = attachmentVersion;
		let generation = endGeneration;
		let revision = queue.revision;
		const ownsEnd = () => !destroyed && generation === endGeneration && intent === positionIntent &&
			queue.isCurrent(revision);
		const ownsOrigin = () => ownsEnd() && matchesOrigin() && ownsEnd();
		const base = queue.snapshot;
		const result = naturalEndPolicy(base, {
			repeat: get(repeat), radioActive: false, continueRadio: false, sleepComplete: false,
			playing: attachment.playing, validEnd: audio.ended && audio.error === null
		});
		if (result.action === 'ignore' || !ownsOrigin()) return;
		attachment.playing = false;
		generation = ++endGeneration;
		announce('');
		if (result.action !== 'play') return;
		const track = currentTrack(result.queue);
		const streamUrl = resolveStreamUrl(track);
		const replacing = currentTrack(base)?.id !== track?.id || source !== streamUrl;
		const { volume, muted, playbackRate } = audio;
		if (!ownsOrigin()) return;
		// Replay supersedes a deferred share even when the native source survives.
		browseCopy.invalidate();
		if (!ownsOrigin()) return;
		pendingShare.clear();
		if (!commit(result.queue) || queue.snapshot !== result.queue || !track || !streamUrl) return;
		revision = queue.revision;
		// A keyed source change retires exactly one node and admits exactly one replacement.
		// Resetting again (including auth reset before mount) cannot inherit this end.
		if (attachmentVersion !== version + Number(replacing)) return;
		await flush();
		const replay = current;
		const isCurrent = () => ownsEnd() && current === replay &&
			attachmentVersion === version + (replacing ? 2 : 0);
		if (!replay || replay.playing) return;
		const target = replay.audio;
		const matches = () => matchesSource(target, streamUrl, isCurrent);
		try {
			if (!matches()) return;
			target.volume = volume;
			if (!matches()) return;
			target.muted = muted;
			if (!matches()) return;
			target.playbackRate = playbackRate;
			runPosition(() => { if (matches()) target.currentTime = 0; });
			if (!matches()) return;
			clearJumpFeedback();
			const play = target.play;
			if (!matches()) return;
			await play.call(target);
			// A captured play promise can fulfill before the queued pause event runs.
			if (!matches() || target.paused || target.ended || target.error !== null) return;
			replay.playing = true;
			announce(`Playing ${shortTitle(displayTitle(track))}.`, true);
		} catch {
			// Autoplay can be refused; native controls remain available, no success claim.
			if (matches()) replay.playing = false;
		}
	}

	return {
		repeat: { subscribe: repeat.subscribe },
		seekFeedback: { subscribe: seekFeedback.subscribe },
		playbackStatus: { subscribe: playbackStatus.subscribe },
		playCurrent, pauseCurrent, retryCurrent: () => startCurrent(true),
		clearSeekFeedback,
		restoreRepeat(storage: Pick<Storage, 'getItem'> | null): void {
			let value: string | null | undefined;
			try { value = storage?.getItem(REPEAT_KEY); } catch { /* Storage is optional. */ }
			repeat.set(value === 'one' || value === 'queue' ? value : 'off');
		},
		setRepeat(value: string, storage: Pick<Storage, 'setItem'> | null): void {
			if (value !== 'off' && value !== 'one' && value !== 'queue') return;
			repeat.set(value);
			try { storage?.setItem(REPEAT_KEY, value); } catch { /* Keep the choice for this visit. */ }
		},
		playSelected,
		restart,
		jump,
		beginShareInput() { pendingShare.clear(); return ++positionIntent; },
		captureShareSeek,
		clearShareSeek: pendingShare.clear,
		get pendingShareSeek() { return pendingShare.current; },
		get attachmentVersion() { return attachmentVersion; },
		get audio() { return current?.audio; },
		get attachment(): MediaAttachment | undefined { return current; },
		attach(audio: HTMLAudioElement): () => void {
			if (destroyed || queue.purging) return () => {};
			if (mountedAudio.has(audio)) throw new Error('Native observations require a fresh audio element');
			mountedAudio.add(audio);
			const previous = current;
			if (previous) pendingShare.clear();
			attachmentVersion += 1;
			// Node identity is not attachment identity, even for the same source.
			const receipt = { audio, playing: false, listeners: new AbortController() };
			current = receipt;
			previous?.listeners.abort();
			if (previous) invalidatePlay();
			function listenForNativeEvents(): void {
				const observe = (event: Event) => {
					if (current !== receipt || event.currentTarget !== audio) return;
					const shareRevision = pendingShare.revision;
					const intent = positionIntent;
					const source = selectedSource();
					if (!source) return;
					// Same-source queue edits leave this attachment and its events current.
					const matches = (generation?: number) => matchesSource(audio, source, () =>
						current === receipt && selectedSource() === source && (generation === undefined || positionIntent === generation));
					if (!matches()) return;
					switch (event.type) {
						case 'play':
						case 'playing':
							if (!audio.paused && !audio.ended && matches()) {
								receipt.playing = true;
								if (!pendingPlay && (event.type === 'playing' || get(playbackStatus) !== 'playing')) {
									readPlayback(audio, matches, event.type === 'playing' ? 'playing' : 'starting');
								}
							}
							return;
						case 'emptied':
							// A native recovery outside the compact action can also clear a resource error.
							if (get(playbackStatus) === 'error') readPlayback(audio, matches, 'starting');
							return;
						case 'pause':
							// Native completion pauses before `ended`; retain that intent.
							if (!audio.paused || audio.ended) return;
							break;
						case 'error':
							if (audio.error === null) return;
							break;
						case 'ended': return handleEnded(receipt, source, matches);
						case 'seeking':
							if (shareRevision === pendingShare.revision) pendingShare.observeSeeking(audio);
							return observeSeeking(audio, () => matches(intent));
						case 'loadedmetadata':
						case 'durationchange':
							retryShare(audio, shareRevision);
							if (!matches()) return;
							// fall through to refresh position availability
						default: {
							const observedIntent = positionIntent;
							const restartCanSeek = restartIsAvailable(audio, () => matches(observedIntent));
							if (!matches(observedIntent)) return;
							const feedback = get(seekFeedback);
							if (restartCanSeek !== feedback.restartCanSeek) seekFeedback.set({ ...feedback, restartCanSeek });
							return;
						}
					}
					if (!matches()) return;
					receipt.playing = false;
					endGeneration += 1;
					// A fatal decoder error may never settle the native play promise.
					if (audio.error) settlePlay('error');
					else invalidatePlay();
					readPlayback(audio, matches, get(playbackStatus));
				};
				for (const type of ['play', 'playing', 'pause', 'error', 'ended', 'emptied', 'seeking',
					'loadedmetadata', 'durationchange', 'canplay', 'progress', 'timeupdate']) {
					audio.addEventListener(type, observe, { signal: receipt.listeners.signal });
				}
			}
			lastJumpTarget = undefined;
			listenForNativeEvents();
			const target = transportTarget();
			if (target?.attachment === receipt) {
				readPlayback(audio, target.matches, 'starting');
				bindSystemTransport(queue, () => target.matches() ? currentTrack(queue.snapshot) : null,
					{ playCurrent, pauseCurrent }, receipt.listeners.signal);
			}
			pause(previous?.audio);
			return () => {
				if (current === receipt) reset();
			};
		},
		reset, purgeAtAuthBoundary,
		destroy() {
			destroyed = true;
			pendingShare.clear();
			reset();
		}
	};
}
