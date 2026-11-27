import { get } from 'svelte/store';
import type ShortcutHelp from './ShortcutHelp.svelte';
import type { createMediaController } from './mediaController.js';
import { actionForShortcutEvent, type ShortcutAction } from './shortcuts.js';

type Media = ReturnType<typeof createMediaController>;
type Help = ReturnType<typeof ShortcutHelp>;

const handlers: Partial<Record<ShortcutAction, (media: Media, help: Help) => boolean | void>> = {
	'toggle-play': (media) => {
		const status = get(media.playbackStatus);
		if (status === 'playing' || status === 'starting') media.pauseCurrent();
		else if (status === 'paused' || status === 'blocked') void media.playCurrent();
		else return false;
	},
	'restart-track': (media) => {
		if (!get(media.seekFeedback).restartCanSeek) return false;
		media.restart();
	},
	'seek-back': (media) => media.jump('back'),
	'seek-forward': (media) => media.jump('forward'),
	'open-shortcuts': (_, help) => { void help.open(); }
};

// Escape is listed but handled by the topmost native dialog's cancel event.
export const appShortcutActions = [...Object.keys(handlers), 'close-shortcuts'] as ShortcutAction[];

export function handleAppShortcut(event: KeyboardEvent, media: Media, help: Help | undefined): void {
	if (!help || help.isOpen() || event.repeat || (event.shiftKey && event.key !== '?')) return;
	const scope = event.view?.document ?? globalThis.document;
	if (!scope) return;
	const action = actionForShortcutEvent(event, scope);
	const handler = action ? handlers[action] : undefined;
	if (handler && handler(media, help) !== false) event.preventDefault();
}
