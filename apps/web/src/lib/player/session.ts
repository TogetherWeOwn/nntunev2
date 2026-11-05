/** Per-layout playback ownership. Never instantiate this store at module scope. */
import { writable } from 'svelte/store';
import {
	createQueue, currentTrack, naturalEndPolicy, nextTrack, playNext, previousTrack, shuffleUpcoming,
	type QueueState, type RepeatMode
} from './queue.js';
import { createTransport, type Transport, type TransportSnapshot } from './transport.js';
import type { SnapshotStorage } from '../library/cache.js';
import type { SleepTimerMode } from './sleepTimer.js';
import { resolveStreamUrl } from '../library/stream-url.js';
import { displayTitle } from '../library/order.js';
import { restartIsAvailable, restartNative } from './nativeRestart.js';
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
		restartAvailable: false, restartStatus: ''
	};
}

export function createPlayerSession(deps: PlayerSessionDeps) {
	let state: PlayerSessionSnapshot = {
		...emptyQueueView(0, true), repeat: 'off', transport: null,
		announcementVersion: 0, queueFeedbackVersion: 0, restartVersion: 0
	};
	const store = writable(state);
	let storage: SnapshotStorage | null = null;
	let favoritesSession: SnapshotStorage | null = null;
	let favoritesInitialized = false;
	let transport: Transport | null = null;
	let generation = 0;
	let started = false;
	let pendingNavigation: { revision: number; keepPlaying: boolean } | null = null;
	const queueExport = createQueueExport({
		getQueue: () => state.queue,
		isActive: () => started
	});
	const beforeRestart = new Set<(trackId: string, source: string) => void>();
	const beforeNavigate = new Set<(next: QueueState) => void>();
	const afterQueuedFavorite = new Set<(id: string) => void>();
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

	function restart(): boolean {
		const audio = matchingAudio();
		const selected = currentTrack(state.queue);
		if (!started || !audio || !selected) {
			publish({ restartAvailable: false, restartStatus: '' });
			return false;
		}
		// Invalidate deferred share and resume seeks before the native write,
		// including a refused write. Metadata must not undo explicit intent.
		for (const listener of beforeRestart) listener(selected.id, selected.streamUrl);
		if (!started || matchingAudio() !== audio) return false;
		transport?.cancelPendingResume();
		const text = restartNative(audio, displayTitle(selected));
		publish({ restartStatus: text, restartAvailable: restartIsAvailable(audio), restartVersion: state.restartVersion + 1 });
		return !text.startsWith('Could not restart');
	}

	function publish(patch: Partial<PlayerSessionSnapshot>): void {
		state = { ...state, ...patch };
		store.set(state);
	}

	/** Bootstrap only after an authenticated catalogue has minted its context. */
	function initializeFavorites(): void {
		if (!started || favoritesInitialized) return;
		favoritesInitialized = true;
		publish({ favorites: loadFavorites(favoritesSession, storage) });
	}

	/** Memory remains authoritative across routes and failed writes this visit. */
	function setFavorites(favorites: string[]): boolean {
		if (!started || !favoritesInitialized) return false;
		publish({ favorites });
		saveFavorites(favoritesSession, storage, favorites);
		return true;
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
		if (!toggleFavorite(id)) return false;
		setQueueFeedback(`${wasFavorite ? 'Unstarred' : 'Starred'} ${track.title}`);
		for (const listener of afterQueuedFavorite) listener(id);
		return true;
	}

	/** A persistent player action must not invoke queue-row focus aftermath. */
	function toggleCurrentFavorite(): boolean {
		const track = currentTrack(state.queue);
		if (!track) return false;
		const wasFavorite = state.favorites.includes(track.id);
		if (!toggleFavorite(track.id)) return false;
		setQueueFeedback(wasFavorite
			? `Removed ${displayTitle(track)} from Favorites.`
			: `Saved ${displayTitle(track)} to Favorites.`);
		return true;
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

	function setQueue(queue: QueueState): boolean {
		if (queue === state.queue) return true;
		const revision = generation;
		queueExport.reset();
		// Store notifications may synchronously replace the source or account.
		if (generation !== revision) return false;
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
		if (changedSource) transport?.disconnect();
		if (generation !== revision + 1) return false;
		publish({ queue, queueSelection: reconcileQueueSelection(state.queueSelection, queue, state.queueFilterQuery),
			...(changedSource ? { audio: null, restartAvailable: false, restartStatus: '' } : {}), announcement: '', queueFeedback: '',
			queueShareStatus: '', queueShareFallbackUrl: null,
			...(queue.tracks.length ? { queueFeedbackExposed: true } : {}) });
		return generation === revision + 1;
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
		for (const listener of beforeNavigate) listener(queue);
		return started && revision === generation;
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
		const result = naturalEndPolicy(state.queue, {
			repeat: state.repeat, radioActive: false, continueRadio: false,
			playing: true, validEnd: true, sleepComplete: false
		});
		if (result.action !== 'play') return;
		if (!prepareNavigation(result.queue)) return;
		if (!setQueue(result.queue)) return;
		const revision = generation;
		const selected = currentTrack(state.queue);
		await deps.flush();
		if (!started || revision !== generation || !selected || !state.audio ||
			state.audio.getAttribute('src') !== selected.streamUrl) return;
		// Repeat is an explicit restart, not a restore of the just-completed position.
		if (!transport?.seekToStart()) return;
		await transport.play();
		if (revision !== generation || state.transport?.status !== 'playing') return;
		const title = Array.from(selected.title.trim() || selected.id).slice(0, 60).join('');
		publish({ announcement: `Playing ${title}.`, announcementVersion: state.announcementVersion + 1,
			queueFeedback: `Playing ${title}.`, queueFeedbackVersion: state.queueFeedbackVersion + 1 });
	}

	function attach(audio: HTMLAudioElement): () => void {
		const selected = currentTrack(state.queue);
		if (!selected || !selected.streamUrl || audio.getAttribute('src') !== selected.streamUrl) return () => {};
		publish({ audio });
		transport?.connect(audio, selected);
		const refreshRestart = () => {
			if (state.audio === audio) publish({ restartAvailable: matchingAudio() === audio && restartIsAvailable(audio) });
		};
		const events = ['loadedmetadata', 'durationchange', 'canplay', 'progress', 'timeupdate'];
		for (const event of events) audio.addEventListener(event, refreshRestart);
		refreshRestart();
		return () => {
			for (const event of events) audio.removeEventListener(event, refreshRestart);
			if (state.audio !== audio) return;
			++generation;
			pendingNavigation = null;
			transport?.disconnect();
			publish({ audio: null, restartAvailable: false, restartStatus: '' });
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
			onchange: (snapshot) => publish({ transport: snapshot }),
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
		beforeNavigate.clear();
		afterQueuedFavorite.clear();
		publish({ ...emptyQueueView(state.queueConfirmGeneration + 1), transport: null });
	}

	return {
		subscribe: store.subscribe,
		snapshot: () => state,
		queueExport,
		start, attach, setQueue, setQueueFeedback, exposeQueueFeedback, setRepeat, resetAtAuthBoundary, destroy,
		next, selectNext, previous, editQueue, setQueueFilter, setQueueLiveRegions, invalidateQueueConsent,
		initializeFavorites, setFavorites, toggleFavorite, toggleQueuedFavorite, toggleCurrentFavorite,
		toggleQueueSelection: (id: string, range = false) => updateQueueSelection(toggleQueueSelection(state.queueSelection, state.queue, state.queueFilterQuery, id, range)),
		selectAllQueueRows: () => updateQueueSelection(selectAllQueueRows(state.queueSelection, state.queue, state.queueFilterQuery)),
		editQueueSelection: (action: QueueSelectionAction) => {
			const before = state.queue;
			const queue = applyQueueSelection(before, state.queueSelection.ids, action);
			const count = action === 'remove' ? before.tracks.length - queue.tracks.length
				: state.queueSelection.ids.filter((id) => id !== currentTrack(queue)?.id).length;
			if (queue === before || !rearrangeQueue(() => queue)) return false;
			const tracks = `${count} ${count === 1 ? 'track' : 'tracks'}`;
			setQueueFeedback(action === 'remove' ? `Removed ${tracks} from the queue.` : `Moved ${tracks} to play next.`, true);
			return true;
		},
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
		restart,
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
