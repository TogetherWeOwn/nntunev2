export interface TabTitleTrack {
	id: string;
	title: string;
	artist: string;
}

export interface DemoTabTitle {
	readonly originalTitle: string;
	update(track: TabTitleTrack | null, playing: boolean): void;
	restore(): void;
}

export const TAB_TITLE_PAUSED_SUFFIX = ' (paused)';
const titleOwners = new WeakMap<{ title: string }, object>();

/** Blank metadata uses display fallbacks; non-blank text stays verbatim. */
export function formatNowPlayingTitle(track: TabTitleTrack, playing: boolean): string {
	const title = (track.title ?? '').trim() === '' ? track.id : track.title;
	const artist = (track.artist ?? '').trim() === '' ? '' : ` — ${track.artist}`;
	return `${title}${artist}${playing ? '' : TAB_TITLE_PAUSED_SUFFIX}`;
}

export function resolveDemoTabTitle(
	originalTitle: string,
	track: TabTitleTrack | null,
	playing: boolean
): string {
	return track === null ? originalTitle : formatNowPlayingTitle(track, playing);
}

/** DOM-free controller: the browser caller supplies its document at mount. */
export function createDemoTabTitle(target: { title: string }): DemoTabTitle {
	const originalTitle = target.title;
	const owner = {};
	titleOwners.set(target, owner);
	let restored = false;
	return {
		originalTitle,
		update(track, playing) {
			if (titleOwners.get(target) !== owner) return;
			target.title = resolveDemoTabTitle(originalTitle, track, playing);
		},
		restore() {
			if (restored || titleOwners.get(target) !== owner) return;
			restored = true;
			target.title = originalTitle;
		}
	};
}
