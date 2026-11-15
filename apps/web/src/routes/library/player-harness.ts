import ts from 'typescript';
import { currentTrack } from '#lib/player/queue.js';
import { createPlayerSession, type PlayerSessionSnapshot } from '#lib/player/session.js';

/**
 * Adapter for extracted Library scripts. Queue/view policy runs in the real
 * per-layout session; only Svelte subscriptions and native audio attachment
 * are replaced. Minimal seek doubles used by share tests are not decoders.
 */
export interface PlayerAudio {
	paused?: boolean;
	pause?: () => void;
}

export interface PlayerFacadeState extends Omit<PlayerSessionSnapshot, 'audio'> {
	audio: PlayerAudio | null;
}

export function createPlayerFacade(initial: Partial<PlayerFacadeState> = {}) {
	const session = createPlayerSession({ flush: () => Promise.resolve() });
	session.start(null);
	if (initial.queue) session.setQueue(initial.queue);
	if (initial.repeat) session.setRepeat(initial.repeat);
	if (initial.queueFeedback) session.setQueueFeedback(initial.queueFeedback);
	let audio = initial.audio ?? null;
	let state: PlayerFacadeState = { ...session.snapshot(), audio };
	const observers = new Set<(state: PlayerFacadeState) => void>();
	const notify = () => {
		for (const observer of observers) observer(state);
	};
	session.subscribe((snapshot) => {
		const before = currentTrack(state.queue);
		const after = currentTrack(snapshot.queue);
		if (before?.id !== after?.id || before?.streamUrl !== after?.streamUrl) audio = null;
		state = { ...snapshot, audio };
		notify();
	});
	return {
		...session,
		get: () => state,
		observe(observer: (state: PlayerFacadeState) => void): () => void {
			observers.add(observer);
			return () => observers.delete(observer);
		},
		/** Minimal native seek surface; real attachment is covered in browsers. */
		attach(element: PlayerAudio): void {
			audio = element;
			state = { ...state, audio };
			notify();
		},
		resetAtAuthBoundary(): void {
			if (audio && !audio.paused) audio.pause?.();
			audio = null;
			session.resetAtAuthBoundary();
		}
	};
}

export type PlayerFacade = ReturnType<typeof createPlayerFacade>;

/** Route code that mirrors facade state into the extracted script's locals. */
export const MIRROR_QUEUE = `player.observe((s) => { queue = s.queue; });`;

/**
 * For whole-script harnesses: `const x = $derived(... $player ...)` cells are
 * lifted to plain `let` bindings and re-evaluated whenever the facade changes.
 * Returns the rewritten statements plus the sync code to append.
 */
export function liftPlayerProjections(
	parsed: ts.SourceFile, statements: readonly ts.Statement[]
): { body: string; sync: string } {
	const lifted: { name: string; expression: string }[] = [];
	const body = statements.map((statement) => {
		if (!ts.isVariableStatement(statement)) return statement.getText(parsed);
		const declaration = statement.declarationList.declarations[0];
		const value = declaration?.initializer;
		if (!value || !ts.isCallExpression(value) || value.expression.getText(parsed) !== '$derived' ||
			!value.getText(parsed).includes('$player') || !ts.isIdentifier(declaration.name)) {
			return statement.getText(parsed);
		}
		const expression = value.arguments[0].getText(parsed);
		lifted.push({ name: declaration.name.text, expression });
		return `let ${declaration.name.text} = ${expression};`;
	}).join('\n');
	const assignments = lifted.map(({ name, expression }) => `${name} = ${expression};`).join(' ');
	return {
		body,
		sync: `__facade.observe((s) => { $player = s; ${assignments} });`
	};
}
