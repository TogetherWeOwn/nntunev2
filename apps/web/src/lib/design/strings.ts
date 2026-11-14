import type { IconName } from './icons.js';

/**
 * String-key table for design-layer copy. Keys ship now so localisation
 * is a translation file later, not a retrofit: each key is a stable
 * dotted path, each value below is the English default.
 *
 * Icon entries are fallback accessible names only. Actionable hosts pass
 * the visible action name (Play, Next track, Save to Favorites) as the
 * label prop instead; the table keeps decorative-but-named uses and
 * automated checks honest in the meantime.
 */
export type DesignStringKey = `design.icon.${IconName}`;

export const DESIGN_STRINGS_EN: Record<DesignStringKey, string> = {
	'design.icon.play': 'Play',
	'design.icon.pause': 'Pause',
	'design.icon.skip-back': 'Previous track',
	'design.icon.skip-forward': 'Next track',
	'design.icon.restart': 'Restart current track',
	'design.icon.shuffle': 'Shuffle',
	'design.icon.repeat': 'Repeat',
	'design.icon.repeat-1': 'Repeat one',
	'design.icon.heart': 'Save to Favorites',
	'design.icon.heart-filled': 'Remove from Favorites',
	'design.icon.plus': 'Add',
	'design.icon.check': 'Confirm',
	'design.icon.x': 'Close',
	'design.icon.chevron-left': 'Back',
	'design.icon.chevron-right': 'Forward',
	'design.icon.chevron-down': 'Expand',
	'design.icon.arrow-left': 'Back',
	'design.icon.arrow-right': 'Forward',
	'design.icon.search': 'Search',
	'design.icon.house': 'Home',
	'design.icon.library': 'Your Library',
	'design.icon.list-music': 'Playlist',
	'design.icon.disc-3': 'Album',
	'design.icon.mic-vocal': 'Artist',
	'design.icon.user': 'Account',
	'design.icon.settings': 'Settings',
	'design.icon.bell': 'Notifications',
	'design.icon.volume-1': 'Low volume',
	'design.icon.volume-2': 'Volume',
	'design.icon.volume-x': 'Mute',
	'design.icon.list-video': 'Queue',
	'design.icon.lyrics': 'Lyrics',
	'design.icon.maximize': 'Expand',
	'design.icon.minimize': 'Collapse',
	'design.icon.more-horizontal': 'More options',
	'design.icon.more-vertical': 'More options',
	'design.icon.download': 'Download',
	'design.icon.upload': 'Upload',
	'design.icon.clock': 'History',
	'design.icon.timer': 'Sleep timer',
	'design.icon.gauge': 'Playback speed',
	'design.icon.pin': 'Pin',
	'design.icon.folder': 'Folder',
	'design.icon.trash-2': 'Delete',
	'design.icon.pencil': 'Rename',
	'design.icon.alert-triangle': 'Warning',
	'design.icon.wifi-off': 'Offline',
	'design.icon.refresh-cw': 'Retry',
	'design.icon.grip-vertical': 'Reorder',
	'design.icon.video': 'Video'
};

export function designString(key: DesignStringKey): string {
	return DESIGN_STRINGS_EN[key];
}
