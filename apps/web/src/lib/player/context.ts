import { getContext, setContext, tick } from 'svelte';
import { createPlayerSession, type PlayerSession } from './session.js';

const PLAYER_SESSION = Symbol('player-session');

export function providePlayerSession(session: PlayerSession): void {
	setContext(PLAYER_SESSION, session);
}

export function usePlayerSession(): PlayerSession {
	// Isolated route renders (including SSR tests) get their own inert owner,
	// never a process-wide fallback shared between requests.
	return getContext<PlayerSession>(PLAYER_SESSION) ?? createPlayerSession({ flush: tick });
}
