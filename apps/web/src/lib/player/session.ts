/** Per-layout playback ownership. Never instantiate this store at module scope. */
import { writable } from 'svelte/store';
import {
	createQueue, currentTrack, insertTracks, naturalEndPolicy, nextTrack, playNext, previousTrack, shuffleUpcoming,
	type QueueState, type RepeatMode, type Track
} from './queue.js';
import { createTransport, type Transport, type TransportSnapshot } from './transport.js';
import type { SnapshotStorage } from '../library/cache.js';
import type { SleepTimerMode } from './sleepTimer.js';
import { resolveStreamUrl } from '../library/stream-url.js';
import { displayTitle } from '../library/order.js';
import { restartIsAvailable, restartNative, type NativeResetBoundary } from './nativeRestart.js';
import { describeJump, jumpMedia, type JumpDirection } from './relativeJump.js';
import { applyQueueAction, type QueueAction } from './queueActions.js';
import { createQueueShare } from './queueShare.js';
import { createQueueExport } from './queueExport.js';
import { clearFavoritesAtAuthBoundary, loadFavorites, saveFavorites } from '../library/favorites.js';
import { applyQueueSelection, createQueueSelection, reconcileQueueSelection, selectAllQueueRows, toggleQueueSelection, type QueueSelection, type QueueSelectionAction } from './queueSelection.js';

const REPEAT_KEY = 'nntune:library-repeat:v1';

export interface PlayerSessionSnapshot {
	queue: QueueState;
	favorites: string[];
	repeat: RepeatMode;
	audio: HTMLAudioElement | null;
	transport: TransportSnapshot | null;
	announcement: string;
	announcementVersion: number;
	queueFeedback: string;
	queueFeedbackVersion: number;
	queueFeedbackExposed: boolean;
	queueShareStatus: string;
	queueShareFallbackUrl: string | null;
	queueFilterQuery: string;
	queueSelection: QueueSelection;
	queueDensity: 'comfortable' | 'compact';
	queueLiveRegions: boolean;
	queueConfirmGeneration: number;
	restartAvailable: boolean;
	restartStatus: string;
	restartVersion: number;
	jumpStatus: string;
	jumpStatusVersion: number;
}

export interface PlayerSessionDeps {
	/** Svelte's tick in the app; injectable so source replacement races can be tested. */
	flush: () => Promise<void>;
}

function emptyQueueView(queueConfirmGeneration = 0, queueFeedbackExposed = false) {
	return {
		queue: createQueue([]), favorites: [] as string[], audio: null,
		announcement: '', queueFeedback: '', queueFeedbackExposed, queueConfirmGeneration,
		queueShareStatus: '', queueShareFallbackUrl: null,
		queueFilterQuery: '', queueSelection: createQueueSelection(), queueDensity: 'comfortable' as const, queueLiveRegions: true,
		restartAvailable: false, restartStatus: '', jumpStatus: ''
	};
}

export function createPlayerSession(deps: PlayerSessionDeps) {
	let state: PlayerSessionSnapshot = {
		...emptyQueueView(0, true), repeat: 'off', transport: null,
		announcementVersion: 0, queueFeedbackVersion: 0, restartVersion: 0, jumpStatusVersion: 0
	};
	const store = writable(state);
	let storage: SnapshotStorage | null = null;
	let favoritesSession: SnapshotStorage | null = null;
	let favoritesInitialized = false;
	let transport: Transport | null = null;
	let generation = 0;
	let sourceOwner = {};
	let attachmentOwner = {};
	let positionIntent = 0;
	let jumpSeek: { audio: HTMLAudioElement; owner: object; target: number } | null = null;
	// Detached, authenticated fallback evidence only; never part of the snapshot.
	let queuedSources = new Map<string, { streamUrl: string }>();
	let connecting = false;
	let started = false;
	let pendingNavigation: { revision: number; keepPlaying: boolean } | null = null;
	const queueExport = createQueueExport({
		getQueue: () => state.queue,
		isActive: () => started
	});
	const beforeRestart = new Set<(trackId: string, source: string) => void>();
	const beforePositionIntent = new Set<(trackId: string, source: string) => void>();
	const beforeNavigate = new Set<(next: QueueState) => void>();
	const afterQueuedFavorite = new Set<(id: string) => void>();
	// Mounted producer action only; never consulted for queued playback.
	const queueInsert = new Set<(ids: readonly string[], beforeId: string | null) => void>();
	const queueShare = createQueueShare({
		getQueue: () => state.queue,
		getAudio: matchingAudio,
		getOrigin: () => window.location.origin,
		getClipboard: () => navigator.clipboard,
		onOutcome: (queueShareStatus, queueShareFallbackUrl) => publish({ queueShareStatus, queueShareFallbackUrl })
	});

	function matchingAudio(): HTMLAudioElement | null {
		const selected = currentTrack(state.queue);
		return selected?.streamUrl && state.audio?.isConnected !== false &&
			state.audio?.getAttribute('src') === selected.streamUrl ? state.audio : null;
	}

	function positionAction() {
		const intent = ++positionIntent;
		const revision = generation;
		const owner = sourceOwner;
		const attachment = attachmentOwner;
		const audio = state.audio;
		const selected = currentTrack(state.queue);
		const id = selected?.id;
		const source = selected?.streamUrl;
		const isCurrent = () => started && intent === positionIntent && revision === generation &&
			owner === sourceOwner && attachment === attachmentOwner && state.audio === audio &&
			currentTrack(state.queue)?.id === id && currentTrack(state.queue)?.streamUrl === source;
		return { audio, selected, isCurrent,
			live: () => isCurrent() && audio && audio.isConnected !== false &&
				audio.getAttribute('src') === source && isCurrent() ? audio : null };
	}

	function retireJump(): void {
		jumpSeek = null;
		if (state.jumpStatus) publish({ jumpStatus: '' });
	}

	function resetBoundary(isCurrent: () => boolean): NativeResetBoundary {
		return { isCurrent, onAssigned: () => { if (isCurrent()) retireJump(); } };
	}

	function restart(): boolean {
		const action = positionAction();
		const audio = action.live();
		const { selected, isCurrent } = action;
		if (!isCurrent()) return false;
		if (!audio || !selected) {
			publish({ restartAvailable: false, restartStatus: '' });
			return false;
		}
		// Intent cancels deferred seeks even when the native write is refused.
		for (const listener of beforeRestart) {
			listener(selected.id, selected.streamUrl);
			if (!isCurrent()) return false;
		}
		transport?.cancelPendingResume();
		const text = restartNative(audio, displayTitle(selected), resetBoundary(isCurrent));
		if (!isCurrent()) return false;
		const available = restartIsAvailable(audio);
		if (!isCurrent()) return false;
		publish({ restartStatus: text, restartAvailable: available, restartVersion: state.restartVersion + 1 });
		return isCurrent() && !text.startsWith('Could not restart');
	}

	function jumpRelative(direction: JumpDirection): void {
		const action = positionAction();
		const audio = action.live();
		const { selected, isCurrent } = action;
		if (!isCurrent()) return;
		if (audio && selected) {
			for (const listener of beforePositionIntent) {
				listener(selected.id, selected.streamUrl);
				if (!isCurrent()) return;
			}
			transport?.cancelPendingResume();
		}
		// Keep the shipped helper unchanged; fence every potentially reentrant
		// native read/write without letting an old getter authorize a new write.
		const outcome = audio ? jumpMedia({
			get duration() { return isCurrent() ? audio.duration : Number.NaN; },
			get currentTime() { return isCurrent() ? audio.currentTime : Number.NaN; },
			set currentTime(value) { if (isCurrent()) audio.currentTime = value; },
			get seekable() { return audio.seekable; }
		}, direction) : { kind: 'unavailable' as const };
		if (!isCurrent()) return;
		if (audio && outcome.kind === 'jumped') jumpSeek = { audio, owner: attachmentOwner, target: outcome.target };
		publish({ jumpStatus: describeJump(direction, outcome), jumpStatusVersion: state.jumpStatusVersion + 1 });
	}

	function retireJumpForShare(id: string, source: string): void {
		const selected = currentTrack(state.queue);
		if (selected?.id !== id || selected.streamUrl !== source) return;
		const action = positionAction();
		if (action.live() && action.isCurrent()) retireJump();
	}

	function publish(patch: Partial<PlayerSessionSnapshot>): void {
		state = { ...state, ...patch };
		store.set(state);
	}

	/** Bootstrap only after an authenticated catalogue has minted its context. */
	function initializeFavorites(): void {
		if (!started || favoritesInitialized) return;
		favoritesInitialized = true;
		const owner = sourceOwner;
		const revision = generation;
		const before = state.favorites;
		const favorites = loadFavorites(favoritesSession, storage);
		if (started && owner === sourceOwner && favoritesInitialized &&
			generation === revision && state.favorites === before) publish({ favorites });
	}

	/** Memory remains authoritative across routes and failed writes this visit. */
	function setFavorites(favorites: string[]): boolean {
		if (!started || !favoritesInitialized) return false;
		const owner = sourceOwner;
		const revision = generation;
		const current = () => started && favoritesInitialized && owner === sourceOwner &&
			generation === revision && state.favorites === favorites;
		publish({ favorites });
		if (!current()) return false;
		const local = storage;
		// Reading the scoped key can itself retire the action before the write.
		saveFavorites(favoritesSession, local && {
			getItem: (key) => current() ? local.getItem(key) : null,
			setItem: (key, value) => { if (current()) local.setItem(key, value); },
			removeItem: (key) => { if (current()) local.removeItem(key); }
		}, favorites);
		return current();
	}

	function toggleFavorite(id: string): boolean {
		return setFavorites(state.favorites.includes(id)
			? state.favorites.filter((favorite) => favorite !== id)
			: [...state.favorites, id]);
	}

	function toggleQueuedFavorite(id: string): boolean {
		const track = state.queue.tracks.find((track) => track.id === id);
		if (!track) return false;
		const wasFavorite = state.favorites.includes(id);
		const revision = generation;
		if (!toggleFavorite(id)) return false;
		const favorites = state.favorites;
		const current = () => started && generation === revision && state.favorites === favorites;
		setQueueFeedback(`${wasFavorite ? 'Unstarred' : 'Starred'} ${track.title}`);
		if (!current()) return false;
		for (const listener of afterQueuedFavorite) {
			listener(id);
			if (!current()) return false;
		}
		return true;
	}

	/** A persistent player action must not invoke queue-row focus aftermath. */
	function toggleCurrentFavorite(): boolean {
		const track = currentTrack(state.queue);
		if (!track) return false;
		const wasFavorite = state.favorites.includes(track.id);
		const revision = generation;
		if (!toggleFavorite(track.id)) return false;
		const favorites = state.favorites;
		setQueueFeedback(wasFavorite
			? `Removed ${displayTitle(track)} from Favorites.`
			: `Saved ${displayTitle(track)} to Favorites.`);
		return started && generation === revision && state.favorites === favorites;
	}

	/** Transient action feedback belongs to the layout, never storage or a route. */
	function setQueueFeedback(text: string, repeat = false): void {
		publish({ queueFeedback: text, queueFeedbackVersion: state.queueFeedbackVersion + (repeat ? 1 : 0) });
	}

	/** Auth purge retires the old region; a successful catalogue exposes a clean one. */
	function exposeQueueFeedback(): void {
		if (!state.queueFeedbackExposed) publish({ queueFeedbackExposed: true });
	}

	/** View text is layout-local, never persisted or allowed to alter the queue. */
	function setQueueFilter(query: string): void {
		if (started && query !== state.queueFilterQuery) publish({ queueFilterQuery: query,
			queueSelection: reconcileQueueSelection(state.queueSelection, state.queue, query) });
	}

	/** Selection is layout-local intent, not playback or persisted queue state. */
	function updateQueueSelection(selection: QueueSelection): QueueSelection {
		if (started && selection !== state.queueSelection) publish({ queueSelection: selection });
		return state.queueSelection;
	}

	/** A mounted Library refresh temporarily suspends routine status chatter. */
	function setQueueLiveRegions(live: boolean): void {
		if (started && live !== state.queueLiveRegions) publish({ queueLiveRegions: live });
	}

	/** Pruning or inbound navigation can invalidate consent even in one render batch. */
	function invalidateQueueConsent(): void {
		if (started) publish({ queueConfirmGeneration: state.queueConfirmGeneration + 1 });
	}

	/** Only the current authenticated Library commit/action path supplies these values. */
	function sourceInputs(
		queue: QueueState,
		inputs: readonly { id: string; streamUrl: string }[],
		authoritative: boolean
	): Map<string, { streamUrl: string }> {
		const ids = new Set(queue.tracks.map((track) => track.id));
		const sources = authoritative ? new Map<string, { streamUrl: string }>()
			: new Map([...queuedSources].filter(([id]) => ids.has(id)));
		for (const input of inputs) {
			if (ids.has(input.id)) sources.set(input.id, { streamUrl: resolveStreamUrl(input) });
		}
		return sources;
	}

	function setQueue(queue: QueueState, input?: {
		owner: object;
		tracks: readonly { id: string; streamUrl: string }[];
		authoritative?: boolean;
	}): boolean {
		if (input && (!started || input.owner !== sourceOwner)) return false;
		const sources = sourceInputs(queue, input?.tracks ?? [], input?.authoritative ?? false);
		if (queue === state.queue) {
			// A successful refresh can replace evidence without touching a live decoder.
			if (input) queuedSources = sources;
			return true;
		}
		const revision = generation;
		const previousSources = queuedSources;
		queueExport.reset();
		// Store notifications may synchronously replace the source or account.
		if (generation !== revision || queuedSources !== previousSources) return false;
		const tracks = queue.tracks.map((track) => {
			const streamUrl = resolveStreamUrl(track);
			return streamUrl === track.streamUrl ? track : { ...track, streamUrl };
		});
		if (tracks.some((track, index) => track !== queue.tracks[index])) queue = { ...queue, tracks };
		++generation;
		pendingNavigation = null;
		const before = currentTrack(state.queue);
		const after = currentTrack(queue);
		const changedSource = before?.id !== after?.id || before?.streamUrl !== after?.streamUrl;
		if (changedSource) {
			jumpSeek = null;
			attachmentOwner = {};
			transport?.disconnect();
		}
		if (generation !== revision + 1 || queuedSources !== previousSources) return false;
		// Publish the membership/evidence pair atomically, before any subscriber runs.
		queuedSources = sources;
		publish({ queue, queueSelection: reconcileQueueSelection(state.queueSelection, queue, state.queueFilterQuery),
			...(changedSource ? { audio: null, restartAvailable: false, restartStatus: '', jumpStatus: '' } : {}), announcement: '', queueFeedback: '',
			queueShareStatus: '', queueShareFallbackUrl: null,
			...(queue.tracks.length ? { queueFeedbackExposed: true } : {}) });
		return generation === revision + 1 && queuedSources === sources;
	}

	/** The explicit queued Play gesture is the only consumer of fallback evidence. */
	async function playQueuedTrack(id: string, suffix = ''): Promise<void> {
		if (!started || !state.queueLiveRegions) return;
		const index = state.queue.tracks.findIndex((track) => track.id === id);
		const target = state.queue.tracks[index];
		if (!target) return;
		const queuedSource = resolveStreamUrl(target);
		const evidence = queuedSource ? undefined : queuedSources.get(id);
		const source = queuedSource || evidence?.streamUrl || '';
		const owner = sourceOwner;
		const inputCurrent = () => started && owner === sourceOwner &&
			(queuedSource !== '' || queuedSources.get(id) === evidence);
		const tracks = [...state.queue.tracks];
		tracks[index] = { ...target, streamUrl: source };
		const queue = { tracks, currentIndex: index };
		if (!prepareNavigation(queue) || !inputCurrent()) return;
		const revision = generation + 1;
		if (!setQueue(queue) || generation !== revision || !inputCurrent()) return;
		const current = () => inputCurrent() && generation === revision &&
			currentTrack(state.queue)?.id === id && resolveStreamUrl(currentTrack(state.queue)) === source;
		const title = Array.from(target.title.trim() || target.id).slice(0, 60).join('');
		let lead = `Selected ${title}; no stream to play.`;
		if (source) {
			await deps.flush();
			if (!current()) return;
			const audio = matchingAudio();
			lead = `Selected ${title}, paused: playback was blocked.`;
			if (audio) {
				try {
					// Preserve the explicit single native attempt, including repeated Play.
					await audio.play();
					lead = `Playing ${title}.`;
				} catch {
					// Never retry a rejected gesture or treat it as source absence.
				}
				if (state.audio !== audio) return;
			}
		}
		if (current()) setQueueFeedback(suffix ? `${lead} ${suffix}` : lead, true);
	}

	function setRepeat(repeat: RepeatMode): void {
		publish({ repeat });
		try { storage?.setItem(REPEAT_KEY, repeat); } catch { /* Keep the in-memory choice. */ }
	}

	/** Pointer, keyboard and system navigation share source and intent fencing. */
	async function navigate(queue: QueueState): Promise<void> {
		if (queue === state.queue) return;
		const status = state.transport?.status;
		const keepPlaying = pendingNavigation?.keepPlaying ??
			(status === 'playing' || status === 'loading' || status === 'buffering');
		if (!setQueue(queue)) return;
		const revision = generation;
		pendingNavigation = { revision, keepPlaying };
		const selected = currentTrack(state.queue);
		await deps.flush();
		if (!started || revision !== generation || pendingNavigation?.revision !== revision) return;
		const shouldPlay = pendingNavigation.keepPlaying;
		pendingNavigation = null;
		if (!selected || !state.audio || state.audio.getAttribute('src') !== selected.streamUrl) return;
		// A deliberate Next/Previous starts at zero, not the saved resume point.
		if (!transport?.seekToStart()) return;
		if (shouldPlay) await transport.play();
	}

	function prepareNavigation(queue: QueueState): boolean {
		if (!started) return false;
		const revision = generation;
		// Route-owned share/selection work must retire synchronously, including
		// a clamped Next. A listener may itself cross an auth/source boundary.
		setQueueFeedback('');
		if (!started || revision !== generation) return false;
		for (const listener of beforeNavigate) {
			listener(queue);
			if (!started || revision !== generation) return false;
		}
		return true;
	}

	/** Panel edits retire route work but never seek or autoplay the selection. */
	function editQueue(action: QueueAction): boolean {
		const result = applyQueueAction(state.queue, action);
		if (!result || !prepareNavigation(result.queue)) return false;
		const revision = generation;
		if (!setQueue(result.queue) || !started || generation !== revision + 1) return false;
		setQueueFeedback(result.feedback, result.repeat);
		return true;
	}

	/** Silent rearrangements clear old feedback even on a no-op, without transport. */
	function rearrangeQueue(transform: (queue: QueueState) => QueueState): boolean {
		if (!started) return false;
		const revision = generation;
		const queue = transform(state.queue);
		// Injected shuffle draws may synchronously cross a source/auth boundary.
		if (!started || revision !== generation || !prepareNavigation(queue)) return false;
		const changed = queue !== state.queue;
		return setQueue(queue) && changed && started && generation === revision + 1;
	}

	/** Explicit menu operands reuse the selection reducer without replacing selection. */
	function editQueueIds(ids: readonly string[], action: QueueSelectionAction, menu = false): boolean {
		if (menu && !state.queueLiveRegions) return false;
		const before = state.queue;
		const requested = new Set(ids);
		const live = before.tracks.filter((track) => requested.has(track.id)).map((track) => track.id);
		const queue = applyQueueSelection(before, live, action);
		const count = action === 'remove' ? before.tracks.length - queue.tracks.length
			: live.filter((id) => id !== currentTrack(queue)?.id).length;
		if (queue === before || !rearrangeQueue(() => queue)) return false;
		const revision = generation;
		const tracks = `${count} ${count === 1 ? 'track' : 'tracks'}`;
		const feedback = menu
			? action === 'remove' ? `${count} selected ${count === 1 ? 'track removed' : 'tracks removed'} from queue.` : 'Tracks placed next in the queue.'
			: action === 'remove' ? `Removed ${tracks} from the queue.` : `Moved ${tracks} to play next.`;
		setQueueFeedback(feedback, true);
		return started && generation === revision;
	}

	/** Only a live Library action may resolve an external insertion's stable IDs. */
	function insertQueuedIds(ids: readonly string[], beforeId: string | null): void {
		if (!started || !state.queueLiveRegions ||
			(beforeId !== null && !state.queue.tracks.some((track) => track.id === beforeId))) return;
		// There is one mounted producer. A disposed route unregisters this action;
		// accepted queued-source evidence has no dependency on its lifetime.
		const producer = queueInsert.values().next().value;
		producer?.([...new Set(ids)], beforeId);
	}

	function insertQueuedTracks(tracks: readonly Track[], beforeId: string | null, input: {
		owner: object;
		tracks: readonly { id: string; streamUrl: string }[];
	}): boolean {
		if (!started || input.owner !== sourceOwner || !state.queueLiveRegions ||
			(beforeId !== null && !state.queue.tracks.some((track) => track.id === beforeId))) return false;
		const before = state.queue;
		const sources = queuedSources;
		const queue = insertTracks(before, tracks, beforeId);
		if (queue === before || !prepareNavigation(queue) || queuedSources !== sources) return false;
		const revision = generation;
		if (!setQueue(queue, input) || !started || generation !== revision + 1) return false;
		const added = queue.tracks.length - before.tracks.length;
		setQueueFeedback(added ? `Added ${added} to the queue.` : 'Queue order updated.', true);
		return started && generation === revision + 1;
	}

	function reorderQueuedIds(ids: readonly string[], beforeId: string | null): boolean {
		if (!started || !state.queueLiveRegions ||
			(beforeId !== null && !state.queue.tracks.some((track) => track.id === beforeId))) return false;
		const requested = new Set(ids);
		const tracks = state.queue.tracks.filter((track) => requested.has(track.id));
		const queue = insertTracks(state.queue, tracks, beforeId);
		if (queue === state.queue || !rearrangeQueue(() => queue)) return false;
		const revision = generation;
		setQueueFeedback('Queue order updated.', true);
		return started && generation === revision;
	}

	/** Menus toggle the whole requested set; save batches only add missing IDs. */
	function favoriteQueuedIds(ids: readonly string[], mode: 'toggle' | 'add' = 'toggle'): boolean {
		if (!started || !state.queueLiveRegions) return false;
		const live = [...new Set(ids)].filter((id) => state.queue.tracks.some((track) => track.id === id));
		if (!live.length) return false;
		const remove = mode === 'toggle' && live.every((id) => state.favorites.includes(id));
		const changed = live.filter((id) => state.favorites.includes(id) === remove);
		const revision = generation;
		const favorites = remove ? state.favorites.filter((id) => !changed.includes(id)) : [...state.favorites, ...changed];
		const track = state.queue.tracks.find((track) => track.id === live[0])!;
		if (!setFavorites(favorites) || !started || generation !== revision || state.favorites !== favorites) return false;
		const already = live.length - changed.length;
		const count = `${changed.length} ${changed.length === 1 ? 'track' : 'tracks'}`;
		const subject = ids.length === 1 ? displayTitle(track) : count;
		const missing = new Set(ids).size - live.length;
		setQueueFeedback((remove ? `Removed ${subject} from favorites.`
			: `Starred ${subject}${already ? `; ${already} ${already === 1 ? 'was' : 'were'} already starred` : ''}.`) +
			(missing ? ` ${missing} ${missing === 1 ? 'track is' : 'tracks are'} no longer available.` : ''), true);
		return started && generation === revision && state.favorites === favorites;
	}

	/** Metadata-only queue Next preserves the next attachment's saved position. */
	function selectNext(): void {
		const queue = nextTrack(state.queue);
		if (prepareNavigation(queue)) setQueue(queue);
	}

	function next(): Promise<void> {
		const queue = nextTrack(state.queue);
		return prepareNavigation(queue) ? navigate(queue) : Promise.resolve();
	}

	function previous(): Promise<void> {
		if (state.audio && Number.isFinite(state.audio.currentTime) && state.audio.currentTime > 3) {
			if (prepareNavigation(state.queue)) transport?.seekToStart();
			return Promise.resolve();
		}
		const queue = previousTrack(state.queue);
		return prepareNavigation(queue) ? navigate(queue) : Promise.resolve();
	}

	function play(): Promise<void> {
		if (pendingNavigation) {
			pendingNavigation.keepPlaying = true;
			return Promise.resolve();
		}
		return transport?.play() ?? Promise.resolve();
	}

	async function advanceAfterEnd(id: string): Promise<void> {
		if (!started || currentTrack(state.queue)?.id !== id) return;
		const endIntent = ++positionIntent;
		const endedAudio = state.audio;
		const endedAttachment = attachmentOwner;
		const endedSource = currentTrack(state.queue)?.streamUrl;
		const result = naturalEndPolicy(state.queue, {
			repeat: state.repeat, radioActive: false, continueRadio: false,
			playing: true, validEnd: true, sleepComplete: false
		});
		if (result.action !== 'play') return;
		if (!prepareNavigation(result.queue) || positionIntent !== endIntent) return;
		if (!setQueue(result.queue) || positionIntent !== endIntent) return;
		const revision = generation;
		const selected = currentTrack(state.queue);
		await deps.flush();
		if (!started || revision !== generation || positionIntent !== endIntent || !selected) return;
		const sameSource = selected.id === id && selected.streamUrl === endedSource;
		if (sameSource && (state.audio !== endedAudio || attachmentOwner !== endedAttachment)) return;
		const action = positionAction();
		if (!action.live() || !action.isCurrent()) return;
		// A normal zero assignment retires feedback even if readback refuses it.
		if (!transport?.seekToStart(resetBoundary(action.isCurrent)) || !action.isCurrent()) return;
		await transport.play();
		if (!action.isCurrent() || state.transport?.status !== 'playing') return;
		const title = Array.from(selected.title.trim() || selected.id).slice(0, 60).join('');
		publish({ announcement: `Playing ${title}.`, announcementVersion: state.announcementVersion + 1,
			queueFeedback: `Playing ${title}.`, queueFeedbackVersion: state.queueFeedbackVersion + 1 });
	}

	function attach(audio: HTMLAudioElement): () => void {
		const selected = currentTrack(state.queue);
		if (!selected || !selected.streamUrl || audio.getAttribute('src') !== selected.streamUrl) return () => {};
		const revision = generation;
		const owner = attachmentOwner = {};
		jumpSeek = null;
		const attached = () => attachmentOwner === owner && state.audio === audio;
		const current = () => generation === revision && attached() && currentTrack(state.queue) === selected;
		publish({ audio, jumpStatus: '' });
		if (!current()) return () => {};
		const activeTransport = transport;
		if (activeTransport) {
			// connect's internal disconnect publishes before attachment completes.
			// Expose only its complete snapshot, so subscribers cannot retire half a connect.
			connecting = true;
			try { activeTransport.connect(audio, selected); }
			finally { connecting = false; }
			if (!current() || transport !== activeTransport) return () => {};
			publish({ transport: activeTransport.snapshot() });
			if (!current()) return () => {};
		}
		const refreshRestart = () => {
			if (!attached()) return;
			const available = matchingAudio() === audio && restartIsAvailable(audio);
			if (attached()) publish({ restartAvailable: available });
		};
		const seeking = () => {
			if (!attached() || !state.jumpStatus) return;
			const intent = positionIntent;
			const position = audio.currentTime;
			if (!attached() || intent !== positionIntent) return;
			if (jumpSeek?.audio === audio && jumpSeek.owner === owner && Number.isFinite(position) &&
				Math.abs(position - jumpSeek.target) <= 0.05) return;
			retireJump();
		};
		const events = ['loadedmetadata', 'durationchange', 'canplay', 'progress', 'timeupdate'];
		for (const event of events) audio.addEventListener(event, refreshRestart);
		audio.addEventListener('seeking', seeking);
		refreshRestart();
		return () => {
			for (const event of events) audio.removeEventListener(event, refreshRestart);
			audio.removeEventListener('seeking', seeking);
			if (!attached()) return;
			const revision = ++generation;
			++positionIntent;
			jumpSeek = null;
			pendingNavigation = null;
			transport?.disconnect();
			if (generation !== revision || !attached()) return;
			attachmentOwner = {};
			publish({ audio: null, restartAvailable: false, restartStatus: '', jumpStatus: '' });
		};
	}

	/** Called once by the layout, only in the browser. Storage refusal is harmless. */
	function start(nextStorage: SnapshotStorage | null, session: SnapshotStorage | null = null): void {
		if (started) return;
		started = true;
		storage = nextStorage;
		// Auth cleanup needs storage even before a catalogue can bootstrap favorites.
		favoritesSession = session;
		let repeat: RepeatMode = 'off';
		try {
			const saved = storage?.getItem(REPEAT_KEY);
			if (saved === 'one' || saved === 'queue') repeat = saved;
		} catch { /* Defaults remain usable when browser storage is blocked. */ }
		publish({ repeat });
		transport = createTransport({
			storage: {
				getItem: (key) => storage?.getItem(key) ?? null,
				setItem: (key, value) => storage?.setItem(key, value),
				removeItem: (key) => storage?.removeItem(key)
			},
			onchange: (snapshot) => { if (!connecting) publish({ transport: snapshot }); },
			onplay: () => void play(),
			onnext: () => void next(),
			onprevious: () => void previous(),
			onpause: () => {
				// Pause cancels autoplay, not the selected source's required reset.
				if (pendingNavigation) pendingNavigation.keepPlaying = false;
				else ++generation;
				publish({ announcement: '' });
			},
			onended: (id) => void advanceAfterEnd(id)
		});
		const selected = currentTrack(state.queue);
		if (state.audio && selected) transport.connect(state.audio, selected);
	}

	function resetAtAuthBoundary(): void {
		sourceOwner = {};
		attachmentOwner = {};
		++positionIntent;
		jumpSeek = null;
		queuedSources = new Map();
		clearFavoritesAtAuthBoundary(favoritesSession, storage);
		favoritesInitialized = false;
		queueShare.reset();
		queueExport.reset(true);
		++generation;
		pendingNavigation = null;
		// Do not save a previous account's position or keep its armed sleep timer.
		transport?.disconnect(false);
		publish({ ...emptyQueueView(state.queueConfirmGeneration + 1), queueDensity: state.queueDensity,
			announcementVersion: 0, queueFeedbackVersion: 0, restartVersion: 0 });
	}

	function destroy(): void {
		sourceOwner = {};
		attachmentOwner = {};
		++positionIntent;
		jumpSeek = null;
		queuedSources = new Map();
		queueShare.reset();
		queueExport.reset(true);
		++generation;
		pendingNavigation = null;
		started = false;
		transport?.destroy();
		transport = null;
		storage = null;
		favoritesSession = null;
		favoritesInitialized = false;
		beforeRestart.clear();
		beforePositionIntent.clear();
		beforeNavigate.clear();
		afterQueuedFavorite.clear();
		queueInsert.clear();
		publish({ ...emptyQueueView(state.queueConfirmGeneration + 1), transport: null });
	}

	return {
		subscribe: store.subscribe,
		snapshot: () => state,
		queueExport,
		start, attach, setQueue, setQueueFeedback, exposeQueueFeedback, setRepeat, resetAtAuthBoundary, destroy,
		sourceInputOwner: () => sourceOwner,
		playQueuedTrack, insertQueuedIds, insertQueuedTracks, reorderQueuedIds, favoriteQueuedIds,
		onQueueInsert: (listener: (ids: readonly string[], beforeId: string | null) => void) => {
			queueInsert.clear();
			queueInsert.add(listener);
			return () => { queueInsert.delete(listener); };
		},
		next, selectNext, previous, editQueue, setQueueFilter, setQueueLiveRegions, invalidateQueueConsent,
		initializeFavorites, setFavorites, toggleFavorite, toggleQueuedFavorite, toggleCurrentFavorite,
		toggleQueueSelection: (id: string, range = false) => updateQueueSelection(toggleQueueSelection(state.queueSelection, state.queue, state.queueFilterQuery, id, range)),
		selectAllQueueRows: () => updateQueueSelection(selectAllQueueRows(state.queueSelection, state.queue, state.queueFilterQuery)),
		editQueueSelection: (action: QueueSelectionAction) => editQueueIds(state.queueSelection.ids, action),
		removeQueuedIds: (ids: readonly string[]) => editQueueIds(ids, 'remove', true),
		playNextQueuedIds: (ids: readonly string[]) => editQueueIds(ids, 'playNext', true),
		clearQueueSelection: () => {
			if (state.queueSelection.active) updateQueueSelection(createQueueSelection());
		},
		setQueueDensity: (queueDensity: PlayerSessionSnapshot['queueDensity']) => {
			if (queueDensity !== state.queueDensity) publish({ queueDensity });
		},
		copyQueuedTrackLink: (id: string) => started ? queueShare.copy(id) : Promise.resolve(),
		onQueuedFavorite: (listener: (id: string) => void) => {
			afterQueuedFavorite.add(listener);
			return () => { afterQueuedFavorite.delete(listener); };
		},
		onBeforeNavigate: (listener: (next: QueueState) => void) => {
			beforeNavigate.add(listener);
			return () => { beforeNavigate.delete(listener); };
		},
		playNext: (index: number) => rearrangeQueue((queue) => playNext(queue, index)),
		shuffle: (random?: () => number) => rearrangeQueue((queue) => shuffleUpcoming(queue, random)),
		cycleRepeat: () => setRepeat(state.repeat === 'off' ? 'queue' : state.repeat === 'queue' ? 'one' : 'off'),
		play,
		retry: () => transport?.retry() ?? Promise.resolve(),
		pause: () => transport?.pause(),
		togglePlay: () => {
			if (pendingNavigation) {
				if (pendingNavigation.keepPlaying) transport?.pause();
				else pendingNavigation.keepPlaying = true;
			} else transport?.togglePlay();
		},
		seek: (seconds: number) => transport?.seek(seconds) ?? false,
		seekBy: (seconds: number) => transport?.seekBy(seconds) ?? false,
		restart, jumpRelative, retireJumpForShare,
		onBeforePositionIntent: (listener: (trackId: string, source: string) => void) => {
			beforePositionIntent.add(listener);
			return () => { beforePositionIntent.delete(listener); };
		},
		onBeforeRestart: (listener: (trackId: string, source: string) => void) => {
			beforeRestart.add(listener);
			return () => { beforeRestart.delete(listener); };
		},
		setVolume: (volume: number) => transport?.setVolume(volume),
		toggleMute: () => transport?.toggleMute(),
		setSpeed: (rate: number) => transport?.setSpeed(rate),
		armSleep: (mode: SleepTimerMode) => { if (state.audio) transport?.armSleep(mode); },
		cancelSleep: () => transport?.cancelSleep(),
		cancelPendingResume: () => transport?.cancelPendingResume(),
		persistPosition: () => transport?.persistPosition(),
		tick: () => transport?.tick()
	};
}

export type PlayerSession = ReturnType<typeof createPlayerSession>;
