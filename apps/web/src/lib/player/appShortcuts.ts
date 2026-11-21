import { get } from 'svelte/store';
import type ShortcutHelp from './ShortcutHelp.svelte';
import type { createMediaController } from './mediaController.js';
import { actionForShortcutEvent, type ShortcutAction } from './shortcuts.js';

type Media = ReturnType<typeof createMediaController>;
type Help = ReturnType<typeof ShortcutHelp>;

const handlers: Partial<Record<ShortcutAction, (media: Media, help: Help) => boolean | void>> = {
	'toggle-play': (media) => {
		const status = get(media.playbackStatus);
		if (status === 'unavailable' || status === 'error') return false;
		if (status === 'playing' || status === 'starting') media.pauseCurrent();
		else void media.playCurrent();
	},
	'restart-track': (media) => {
		if (!get(media.seekFeedback).restartCanSeek) return false;
		media.restart();
	},
	'seek-back': (media) => media.jump('back'),
	'seek-forward': (media) => media.jump('forward'),
	'open-shortcuts': (_, help) => { void help.open(); },
	'close-shortcuts': (_, help) => help.close()
};

export const appShortcutActions = Object.keys(handlers) as ShortcutAction[];

export function handleAppShortcut(event: KeyboardEvent, media: Media, help: Help | undefined): void {
	if (!help || event.repeat) return;
	// Keep the native media widget's own keyboard handling, including shadow controls.
	if (event.composedPath().some((node) => node instanceof Element && node.matches('audio[controls], video[controls]'))) return;
	const action = actionForShortcutEvent(event, document);
	if (!action || (help.isOpen() && action !== 'close-shortcuts')) return;
	const handler = handlers[action];
	if (handler && handler(media, help) !== false) event.preventDefault();
}
