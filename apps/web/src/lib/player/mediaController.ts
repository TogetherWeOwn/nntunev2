import { get, writable } from 'svelte/store';
import { emptySeek, readAbsoluteSeek, SEEK_READBACK_TOLERANCE, writeAbsoluteSeek, type AbsoluteSeekState } from './absoluteSeek.js';
import { displayTitle, shortTitle } from '../library/order.js';
import { resolveStreamUrl } from '../library/stream-url.js';
import { createPlayerSleepTimer } from './playerSleepTimer.js';
import { currentTrack, naturalEndPolicy, nextTrack, previousTrack, selectTrack, type QueueState, type RepeatMode, type Track } from './queue.js';
import type { createQueueController, QueuedPlaySource } from './queueController.js';
import { describeJump, jumpMedia, RELATIVE_JUMP_SECONDS, type JumpDirection } from './relativeJump.js';
import { createNativeSound } from './nativeSound.js';
import { createPendingShareSeek } from './pendingShareSeek.js';
import { createLivePlaybackSpeed } from './livePlaybackSpeed.js';
import { restartIsAvailable } from './restartAvailability.js';
import type { SystemSeekGuard } from './systemSeek.js';
import { bindSystemTransport } from './systemTransport.js';
import { applyMediaSession, clearMediaSession } from './mediaSession.js';
import type { BrowserPlaybackSource } from './browserPlaybackGrant.js';
import type { ShareTarget } from './shareTimestamp.js';

export type MediaAttachment = { readonly audio: HTMLAudioElement; playing: boolean; soughtToEnd: boolean | { end: number; seekDuration: number; played: TimeRanges; settled?: number; progressed?: boolean; tailProgress?: boolean } };
export type PlaybackStatus = 'unavailable' | 'paused' | 'starting' | 'playing' | 'blocked' | 'error' | 'retrying';

type PlayOutcome = 'playing' | 'blocked' | 'error' | undefined;
type QueueAuthority = Pick<ReturnType<typeof createQueueController>,
	'snapshot' | 'subscribe' | 'revision' | 'revoke' | 'isCurrent' | 'purging' | 'sourceForQueuedPlay'>;

export type MediaSelection = {
	commit: (queue: QueueState, grant?: BrowserPlaybackSource) => boolean;
	streamFor?: (track: Track | null) => string;
	prepare?: (trackId: string, isCurrent: () => boolean, publish: (source: BrowserPlaybackSource) => void,
		options?: { retry?: boolean; retireSuperseded?: () => boolean }) => Promise<boolean>;
	flush: () => Promise<void>;
	browseCopy: { invalidate: () => void };
	announce: (message: string, replay?: boolean) => void;
};

const REPEAT_KEY = 'nntune:library-repeat:v1';
const PREVIOUS_RESTART_SECONDS = 3;
// Native played snapshots are static and normalized (ordered, disjoint, non-touching ranges).
function playedNewTail(before: TimeRanges, after: TimeRanges, end: number, duration: number): boolean {
	for (let i = 0, j = 0; i < (after?.length ?? 0); i++) {
		const start = Math.max(end, after.start(i)), stop = Math.min(duration, after.end(i));
		while (before && j < before.length && before.end(j) <= start) j++;
		if (stop > start && (!before || j === before.length || before.start(j) > start || before.end(j) < stop)) return true;
	}
	return false;
}

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
	let pausedTransport: AbortController | undefined;
	let attachmentVersion = 0;
	let endGeneration = 0;
	let playIntent = 0;
	let grantIntent = 0, pendingGrant: number | undefined;
	let pendingPlay: { result: Promise<PlayOutcome>; finish: (outcome: PlayOutcome) => void } | undefined;
	const playbackStatus = writable<PlaybackStatus>('unavailable');
	const sound = createNativeSound();
	const repeat = writable<RepeatMode>('off');
	const seekFeedback = writable({ restartCanSeek: false, restartStatus: '', restartStatusVersion: 0,
		jumpStatus: '', jumpStatusVersion: 0 });
	const absoluteSeek = writable<AbsoluteSeekState>(emptySeek);
	let seekAnchor = 0;
	let lastJumpTarget: number | undefined;
	let destroyed = false;
	let positionIntent = 0;
	const pendingShare = createPendingShareSeek();
	let deferredShare: { intent: number; target: ShareTarget; current: () => boolean } | undefined;
	let applyingShare: { revision: number; retry: boolean } | undefined;
	let positioning = false;
	let nextPosition: (() => void) | undefined;

	function pause(audio: HTMLAudioElement | undefined): void {
		if (audio && !audio.paused) audio.pause();
	}

	const streamFor = selection?.streamFor ?? resolveStreamUrl;
	const selectedSource = () => streamFor(currentTrack(queue.snapshot));
	const clearShareSeek = () => { deferredShare = undefined; pendingShare.clear(); };
	const sleepTimer = createPlayerSleepTimer(pauseCurrent, () => !destroyed && !queue.purging && !!selectedSource());

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

	const speed = createLivePlaybackSpeed(transportTarget);

	async function startCurrent(retry: boolean, recoverPosition = false): Promise<void> {
		const target = transportTarget();
		if (!target || pendingPlay) return;
		const { audio, matches } = target;
		const trackId = retry ? currentTrack(queue.snapshot)?.id : undefined;
		const sameTrack = () => !retry || currentTrack(queue.snapshot)?.id === trackId;
		const previousIntent = playIntent;
		if (!matches() || (retry ? get(playbackStatus) !== 'error' || !audio.error
			: audio.error || (!audio.paused && !audio.ended)) ||
			!matches(() => playIntent === previousIntent && sameTrack())) return;
		if (trackId && selection?.prepare && !recoverPosition) {
			return preparePlay(trackId, matches, () => startCurrent(true, true), { retry: true });
		}
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
				const rateTarget = { audio, matches: matchesAttempt };
				const liveRate = speed.read(rateTarget);
				let seconds = 0;
				if (recoverPosition) {
					const { attachment, duration, elapsed } = get(absoluteSeek);
					seconds = attachment === attachmentVersion && duration > 0 ? elapsed : audio.currentTime;
				}
				const restore = trackId && Number.isFinite(seconds) && seconds > 0 ?
					captureShareSeek(++positionIntent, { trackId, seconds }, matches) : undefined;
				if (!matchesAttempt()) return;
				load.call(audio);
				restore?.();
				speed.reapply(rateTarget, liveRate); // load() resets playbackRate.
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
		if (selection?.prepare && !selectedSource()) {
			const base = queue.snapshot;
			const id = currentTrack(base)?.id;
			const evidence = id && queue.sourceForQueuedPlay(id);
			if (evidence) return playSelected(base, base.currentIndex, evidence);
		}
		return startCurrent(false);
	}

	function pauseCurrent(): void {
		if (destroyed || queue.purging) return;
		const target = transportTarget();
		if (target && !target.matches()) return;
		invalidatePlay();
		endGeneration += 1;
		// An accepted pause also revokes autoplay waiting for a new attachment.
		if (!target) return;
		const { audio, attachment, matches } = target;
		const intent = playIntent;
		const ownsPause = () => matches(() => playIntent === intent);
		pause(audio);
		if (!ownsPause()) return;
		if (audio.paused || audio.ended) attachment.playing = false;
		readPlayback(audio, ownsPause, get(playbackStatus));
	}

	function nextFromQueue(): void {
		if (!selection) return;
		const base = queue.snapshot;
		const next = nextTrack(base);
		if (next === base) return;
		const transport = selection.prepare && transportTarget();
		if (transport && !transport.audio.paused && !transport.audio.ended && !transport.audio.error &&
			transport.matches(() => queue.snapshot === base)) {
			// Keep the current decoder until the successor's grant is accepted.
			if (pendingGrant !== undefined) return;
			const target = currentTrack(next);
			const source = target && queue.sourceForQueuedPlay(target.id);
			if (source) void playSelected(next, next.currentIndex, source);
			return;
		}
		if (queue.snapshot === base) selection.commit(next);
	}

	function navigateQueue(step: (state: QueueState) => QueueState): void {
		if (!selection) return;
		const base = queue.snapshot;
		const next = step(base);
		if (next === base) return;
		const target = currentTrack(next);
		if (!target) return;
		// Bulk-added slots carry metadata only. Keep the decoder unless Library can resolve the destination.
		const source = queue.sourceForQueuedPlay(target.id, resolveStreamUrl(target) || undefined);
		if (selection.prepare && source?.isCurrent()) return void selection.commit(next);
		if (!source?.streamUrl || !source.isCurrent()) return;
		// A paused handoff hydrates only the destination slot.
		if (target.streamUrl === source.streamUrl) selection.commit(next);
		else {
			const tracks = [...next.tracks];
			tracks[next.currentIndex] = { ...target, streamUrl: source.streamUrl };
			selection.commit({ ...next, tracks });
		}
	}

	function previousFromTransport(): void {
		const transport = selection?.prepare ? transportTarget() : undefined;
		if (!transport) return navigateQueue(previousTrack);
		const base = queue.snapshot;
		const { audio, matches } = transport;
		const sameQueue = () => queue.snapshot === base;
		if (!matches(sameQueue)) return;
		const playing = !audio.paused && !audio.ended && !audio.error;
		const position = audio.currentTime;
		if (!matches(sameQueue)) return;
		if (!playing) return navigateQueue(previousTrack);
		if (position >= PREVIOUS_RESTART_SECONDS) return restart();
		if (pendingGrant !== undefined) return;
		const previous = previousTrack(base);
		if (previous === base) return;
		const target = currentTrack(previous);
		const source = target && queue.sourceForQueuedPlay(target.id);
		if (target && source) void preparePlay(target.id, source.isCurrent,
			(grant) => playSelected(previous, previous.currentIndex, grant, '', grant), { retireSuperseded: true });
	}

	const queueTransport = { playCurrent, pauseCurrent, onNext: () => navigateQueue(nextTrack), onPrevious: previousFromTransport };

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
		pausedTransport?.abort();
		const previous = current;
		current = undefined;
		// Retire the native receipt without imposing replacement's share-clear policy on remove/clear.
		attachmentVersion += 1;
		previous?.listeners.abort();
		invalidatePlay();
		absoluteSeek.set({ ...emptySeek, attachment: attachmentVersion });
		playbackStatus.set('unavailable');
		speed.retire();
		return previous?.audio;
	}

	function reset(): void {
		pause(retire());
	}

	function purgeAtAuthBoundary(clearHint: () => void): void {
		sleepTimer.cancel();
		clearShareSeek();
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

	async function preparePlay(trackId: string, isEligible: () => boolean, publish: (source: BrowserPlaybackSource) => Promise<void>,
		{ retry, retireSuperseded }: { retry?: boolean; retireSuperseded?: boolean } = {}) {
		if (!selection?.prepare || destroyed || queue.purging || !isEligible()) return;
		const request = pendingGrant = ++grantIntent;
		const revision = queue.revision, intent = playIntent, position = positionIntent, version = attachmentVersion;
		const current = () => !destroyed && pendingGrant === request && queue.isCurrent(revision) && isEligible() &&
			intent === playIntent && position === positionIntent && version === attachmentVersion;
		let playback: Promise<void> | undefined;
		try {
			const accepted = await selection.prepare(trackId, current, (grant) => { playback = publish(grant); }, {
				retry, retireSuperseded: retireSuperseded ? () => pendingGrant === request : undefined
			});
			if (!accepted && current()) selection.announce('Could not start playback.', true);
			await playback;
		} finally { if (pendingGrant === request) pendingGrant = undefined; }
	}

	async function playSelected(
		base: QueueState, index: number, source: QueuedPlaySource, suffix = '', grant?: BrowserPlaybackSource
	): Promise<void> {
		if (!selection || queue.purging) return;
		const target = base.tracks[index];
		if (!target || destroyed || !source.isCurrent()) return;
		if (selection.prepare && !grant) return preparePlay(target.id, source.isCurrent, (issued) => playSelected(base, index, issued, suffix, issued));
		const { streamUrl } = source;
		const tracks = [...base.tracks];
		if (!grant) tracks[index] = { ...target, streamUrl };
		if (!selection.commit(selectTrack({ ...base, tracks }, index), grant) || !source.isCurrent() || destroyed) return;
		const share = deferredShare?.target.trackId === target.id ? deferredShare : undefined;
		if (share) deferredShare = undefined;
		const restoreShare = share && captureShareSeek(share.intent, share.target, share.current);

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
			restoreShare?.();
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
		const streamUrl = streamFor(track);
		if (!streamUrl) {
			if (selection?.prepare) deferredShare = { intent, target: { trackId, seconds }, current: routeIsCurrent };
			return;
		}
		// Before the keyed node mounts, only the first upcoming attachment can inherit this seek.
		const version = current ? attachmentVersion : attachmentVersion + 1;
		const revision = pendingShare.set({ trackId, streamUrl, seconds }, (audio) => ({
			isCurrent: () => matchesSource(audio, streamUrl, () => {
				if (!routeIsCurrent() || destroyed || current?.audio !== audio ||
					attachmentVersion !== version || positionIntent !== intent) return false;
				// Other queue edits need not revoke a seek for this track and source.
				const selected = currentTrack(queue.snapshot);
				return selected?.id === trackId && streamFor(selected) === streamUrl;
			})
		}));
		clearJumpFeedback();
		return () => retryShare(current?.audio, revision);
	}

	function onSeekLanded(attachment: MediaAttachment, system?: SystemSeekGuard) {
		if (system || typeof attachment.soughtToEnd === 'object') return (target: number, duration: number, observed: number) => {
			attachment.soughtToEnd = system ? { end: Math.min(duration, Math.max(target, observed)), seekDuration: duration, played: attachment.audio.played } : false;
		};
	}

	function jump(direction: JumpDirection, seconds = RELATIVE_JUMP_SECONDS, system?: SystemSeekGuard): void {
		if (destroyed || queue.purging || system?.active() === false) return;
		const track = currentTrack(queue.snapshot);
		const source = streamFor(track);
		const attachment = current;
		const audio = attachment?.audio;
		const version = attachmentVersion;
		const queueRevision = queue.revision;
		const intent = ++positionIntent;
		// Cancel on acceptance, even if the native seek is deferred or refused.
		if (track) pendingShare.supersede(track.id, source);
		const revision = pendingShare.revision;
		const isCurrent = (guard = system?.active) => !destroyed && attachmentVersion === version && queue.isCurrent(queueRevision) &&
			positionIntent === intent && pendingShare.revision === revision && guard?.() !== false;
		runPosition(() => {
			const matches = (guard = system?.active) => !!audio && !!source && matchesSource(audio, source, () => isCurrent(guard));
			const outcome = attachment && audio && matches() ? jumpMedia(audio, direction, matches, () => {
				if (current === attachment) clearJumpFeedback();
			}, seconds, system?.unavailable, onSeekLanded(attachment, system)) : undefined;
			if (outcome ? !matches(system?.canReport) : !isCurrent()) return;
			if (outcome?.kind === 'jumped') lastJumpTarget = outcome.target;
			seekFeedback.update((feedback) => ({
				...feedback, jumpStatus: describeJump(direction, outcome ?? { kind: 'unavailable' }, seconds),
				jumpStatusVersion: feedback.jumpStatusVersion + 1
			}));
		});
	}

	function observeAbsoluteSeek(audio: HTMLAudioElement, matches: () => boolean, feedback?: string, replay = false, nativeSeeking = false, onUnavailable?: () => void): void {
		const attachment = attachmentVersion;
		const observed = readAbsoluteSeek(audio);
		if (!observed.enabled) onUnavailable?.();
		if (!matches()) return;
		const prior = get(absoluteSeek);
		const previous = prior.attachment === attachment ? prior : emptySeek;
		// An empty or failed private decoder cannot replace the last settled position.
		const observation = selection?.prepare && !observed.duration && previous.duration
			? { ...observed, duration: previous.duration, position: previous.elapsed }
			: observed;
		// In-flight time/progress updates move the thumb, not the seek comparison point.
		const message = feedback ?? (nativeSeeking && observation.position !== seekAnchor ? '' : previous.feedback);
		if (feedback !== undefined || nativeSeeking || !observation.seeking) seekAnchor = observation.position;
		const elapsed = observation.seeking ? previous.elapsed : observation.position;
		if (prior.attachment !== attachment || previous.position !== observation.position ||
			previous.duration !== observation.duration || previous.enabled !== observation.enabled ||
			previous.seeking !== observation.seeking || previous.feedback !== message || (replay && message)) {
			absoluteSeek.set({ ...observation, attachment, elapsed, feedback: message,
				feedbackVersion: previous.feedbackVersion + Number(!!message && (replay || message !== previous.feedback)) });
		}
	}

	function seek(attachment: number, seconds: number, replay = true, system?: SystemSeekGuard): void {
		if (attachment !== attachmentVersion || !Number.isFinite(seconds)) return;
		const target = transportTarget();
		if (!target) return;
		const { audio, matches } = target;
		const before = positionIntent;
		const track = currentTrack(queue.snapshot);
		const sameSelection = () => currentTrack(queue.snapshot)?.id === track?.id;
		if (!track || !matches(() => positionIntent === before && sameSelection() && system?.active() !== false)) return;
		const intent = ++positionIntent;
		pendingShare.supersede(track.id, streamFor(track));
		const isCurrent = (guard = system?.active) => matches(() => positionIntent === intent && sameSelection() && guard?.() !== false);
		runPosition(() => {
			if (!isCurrent()) return;
			const feedback = writeAbsoluteSeek(audio, seconds, isCurrent, system?.unavailable, onSeekLanded(target.attachment, system));
			const canReport = () => isCurrent(system?.canReport);
			if (canReport()) observeAbsoluteSeek(audio, canReport, feedback, replay, false, system?.unavailable);
		});
	}

	function restart(): void {
		const track = currentTrack(queue.snapshot);
		const source = streamFor(track);
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
			if (pendingGrant === undefined) invalidatePlay();
			else settlePlay();
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
		if (!audio.ended || audio.error !== null || !attachment.playing || !ownsOrigin()) return;
		const { soughtToEnd } = attachment;
		if (typeof soughtToEnd === 'object') {
			const { duration, currentTime } = audio, { end, seekDuration, played, progressed, tailProgress, settled } = soughtToEnd, slack = SEEK_READBACK_TOLERANCE;
			const correctedTail = duration < seekDuration && settled !== undefined && (end >= seekDuration || settled >= duration - slack);
			const reachedNewEnd = correctedTail
				? currentTime >= duration - slack && (tailProgress || playedNewTail(played, audio.played, settled, duration))
				: duration > end
				? end < seekDuration || (currentTime >= duration - slack && (currentTime > end + slack || progressed || playedNewTail(played, audio.played, end, duration)))
				: duration === end && currentTime >= duration - slack && (tailProgress || (settled !== undefined && playedNewTail(played, audio.played, settled, duration)));
			if (!ownsOrigin()) return;
			if (!reachedNewEnd) { attachment.playing = attachment.soughtToEnd = false; return; }
		}
		const natural = soughtToEnd !== true;
		attachment.playing = attachment.soughtToEnd = false;
		generation = ++endGeneration;
		announce('');
		if (!ownsOrigin()) return;
		const result = naturalEndPolicy(base, {
			repeat: get(repeat), radioActive: false, continueRadio: false,
			sleepComplete: sleepTimer.observeEnd(natural), playing: true, validEnd: true
		});
		if (result.action !== 'play' || !ownsOrigin()) return;
		const track = currentTrack(result.queue);
		if (selection.prepare && track) {
			if (pendingGrant) return;
			const evidence = queue.sourceForQueuedPlay(track.id);
			if (evidence) await playSelected(result.queue, result.queue.currentIndex, {
				...evidence, isCurrent: () => evidence.isCurrent() && ownsOrigin()
			});
			return;
		}
		const streamUrl = streamFor(track);
		const replacing = currentTrack(base)?.id !== track?.id || source !== streamUrl;
		const playbackRate = speed.observe({ audio, matches: ownsOrigin });
		if (!ownsOrigin()) return;
		// Queue commit captures sound for replacements; replay keeps this attachment.
		if (!replacing) sound.observe();
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
			runPosition(() => { if (matches()) target.currentTime = 0; });
			if (!matches()) return;
			clearJumpFeedback();
			const play = target.play;
			if (replacing && playbackRate !== null) speed.reapply({ audio: target, matches }, playbackRate);
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
		sound: { state: sound.state, setVolume: sound.setVolume, toggleMute: sound.toggleMute },
		observeSound: sound.observe,
		seekFeedback: { subscribe: seekFeedback.subscribe },
		absoluteSeek: { subscribe: absoluteSeek.subscribe }, seek,
		playbackStatus: { subscribe: playbackStatus.subscribe },
		playCurrent, pauseCurrent, nextFromQueue, retryCurrent: () => startCurrent(true),
		bindPausedTransport(track: Track) {
			const lifetime = pausedTransport = new AbortController();
			const guard = (run: () => void) => () => { if (!lifetime.signal.aborted && !queue.purging) run(); };
			if (applyMediaSession({ ...track, artworkUrl: null }, { onPlay: guard(playCurrent),
				onNext: guard(queueTransport.onNext), onPrevious: guard(queueTransport.onPrevious) })) {
				lifetime.signal.addEventListener('abort', () => clearMediaSession(), { once: true });
			}
		},
		speed,
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
		sleepTimer,
		playSelected,
		restart,
		jump,
		beginShareInput() { pendingShare.clear(); return ++positionIntent; },
		captureShareSeek,
		clearShareSeek,
		get pendingShareSeek() { return pendingShare.current; },
		get attachmentVersion() { return attachmentVersion; },
		get audio() { return current?.audio; },
		sharePosition: () => selectedSource() ? current?.audio.currentTime : 0,
		get attachment(): MediaAttachment | undefined { return current; },
		attach(audio: HTMLAudioElement): () => void {
			if (destroyed || queue.purging) return () => {};
			pausedTransport?.abort();
			if (mountedAudio.has(audio)) throw new Error('Native observations require a fresh audio element');
			mountedAudio.add(audio);
			const previous = current;
			if (previous) pendingShare.clear();
			attachmentVersion += 1;
			// Node identity is not attachment identity, even for the same source.
			const receipt: NonNullable<typeof current> = { audio, playing: false, soughtToEnd: false, listeners: new AbortController() };
			current = receipt;
			previous?.listeners.abort();
			if (previous) invalidatePlay();
			const seekTarget = transportTarget();
			const trackId = currentTrack(queue.snapshot)?.id;
			const observePosition = (type?: string) => {
				if (seekTarget?.audio !== audio) return;
				const intent = positionIntent;
				const matches = () => seekTarget.matches(() => positionIntent === intent && currentTrack(queue.snapshot)?.id === trackId);
				if (matches()) observeAbsoluteSeek(audio, matches, undefined, false, type === 'seeking');
			};
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
					const marker = receipt.soughtToEnd;
					if (typeof marker === 'object' && (event.type === 'seeked' || event.type === 'timeupdate') && !audio.seeking) {
						const { currentTime, duration, paused, ended } = audio;
						if (!matches(intent)) return;
						if (event.type === 'seeked') {
							Object.assign(marker, { settled: currentTime, tailProgress: false });
							if (Number.isFinite(duration) && duration > 0 && (duration < marker.seekDuration
								? currentTime >= duration - SEEK_READBACK_TOLERANCE : marker.end < marker.seekDuration && currentTime >= duration)) marker.end = marker.seekDuration = duration;
						} else if (!paused && !ended && currentTime > (marker.settled ?? Infinity) && currentTime < duration) {
							marker.tailProgress = true;
							marker.progressed ||= currentTime > marker.end;
						}
					}
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
							receipt.soughtToEnd = false;
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
						case 'ratechange': return speed.observe({ audio, matches });
						case 'ended': return handleEnded(receipt, source, matches);
						case 'seeked': return;
						case 'seeking': {
							const { currentTime, duration } = audio;
							if (!matches(intent)) return;
							const hasEnd = Number.isFinite(duration) && duration > 0, slack = SEEK_READBACK_TOLERANCE;
							if (typeof marker !== 'object' || !(Math.abs(currentTime - marker.end) <= slack ||
								(marker.settled === undefined && hasEnd && duration < marker.end && Math.abs(currentTime - duration) <= slack)))
								receipt.soughtToEnd = hasEnd && currentTime >= duration;
							if (shareRevision === pendingShare.revision) pendingShare.observeSeeking(audio);
							return observeSeeking(audio, () => matches(intent));
						}
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
				audio.addEventListener('ratechange', observe, { signal: receipt.listeners.signal });
				for (const type of ['play', 'playing', 'pause', 'error', 'ended', 'emptied', 'seeking',
					'loadedmetadata', 'durationchange', 'canplay', 'progress', 'timeupdate', 'seeked']) {
					audio.addEventListener(type, (event) => {
						const settlement = observe(event);
						if (event.currentTarget === audio && type !== 'play' && type !== 'playing' && type !== 'pause') observePosition(type);
						return settlement;
					}, { signal: receipt.listeners.signal });
				}
			}
			lastJumpTarget = undefined;
			listenForNativeEvents();
			speed.attach();
			const target = transportTarget();
			if (target?.attachment === receipt) {
				sound.connect(audio, target.matches, receipt.listeners.signal);
				readPlayback(audio, target.matches, 'starting');
				const attachment = attachmentVersion;
				bindSystemTransport(queue, audio, () => target.matches() ? currentTrack(queue.snapshot) : null,
					selection ? queueTransport : { playCurrent, pauseCurrent }, receipt.listeners.signal, {
						audio, attachment, availability: absoluteSeek, positionIntent: () => positionIntent,
						matches: () => target.matches(() => currentTrack(queue.snapshot)?.id === trackId),
						dispatch: (action, seconds, guard) => action === 'seekto' ? seek(attachment, seconds, true, guard)
							: jump(action === 'seekforward' ? 'forward' : 'back', seconds, guard)
					});
			}
			observePosition();
			pause(previous?.audio);
			return () => {
				if (current === receipt) reset();
			};
		},
		reset, purgeAtAuthBoundary,
		destroy() {
			destroyed = true;
			sleepTimer.cancel();
			clearShareSeek();
			reset();
		}
	};
}
