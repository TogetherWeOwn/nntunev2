/**
 * Inline-SVG icon subset for the shell and player (R1/R2 surfaces).
 * 24-unit grid, `currentColor`: stroked chrome at weight 2 with round
 * caps; solid fills for transport glyphs, dots and the liked heart.
 * Names follow the open-licensed sets in the design spec; every stencil
 * is drawn for this project. Render through `Icon.svelte`. The six
 * R3/R4 stencils (radio, devices, share, link, picture-in-picture,
 * captions) land with their surfaces; the budget test below guards the
 * cap when they do. English fallback names live in `./strings.js`.
 */
export const ICON_NAMES = [
	'play',
	'pause',
	'skip-back',
	'skip-forward',
	'restart',
	'shuffle',
	'repeat',
	'repeat-1',
	'heart',
	'heart-filled',
	'plus',
	'check',
	'x',
	'chevron-left',
	'chevron-right',
	'chevron-down',
	'arrow-left',
	'arrow-right',
	'search',
	'house',
	'library',
	'list-music',
	'disc-3',
	'mic-vocal',
	'user',
	'settings',
	'bell',
	'volume-1',
	'volume-2',
	'volume-x',
	'list-video',
	'lyrics',
	'maximize',
	'minimize',
	'more-horizontal',
	'more-vertical',
	'download',
	'upload',
	'clock',
	'timer',
	'gauge',
	'pin',
	'folder',
	'trash-2',
	'pencil',
	'alert-triangle',
	'wifi-off',
	'refresh-cw',
	'grip-vertical',
	'video'
] as const;

export type IconName = (typeof ICON_NAMES)[number];

const SPK = '<path d="M4 9.5v5h3.5L12 18.5v-13L7.5 9.5H4z"/>';
const F = ' fill="currentColor" stroke="none"';
const HEART =
	'<path d="M12 20.5C7 16.5 3 13.2 3 9.3 3 6.4 5.2 4.5 7.7 4.5c1.7 0 3.3.9 4.3 2.4 1-1.5 2.6-2.4 4.3-2.4 2.5 0 4.7 1.9 4.7 4.8 0 3.9-4 7.2-9 11.2z"/>';

/** Existing player stencils, independently importable without the full manifest. */
export const TRANSPORT_ICON_PATHS = {
	play: `<path d="M7 4.5v15l13-7.5z"${F}/>`,
	pause: `<rect x="6" y="5" width="4" height="14" rx="1"${F}/><rect x="14" y="5" width="4" height="14" rx="1"${F}/>`,
	'skip-back': `<path d="M5 5h2.5v14H5z"${F}/><path d="M19 5v14L9.5 12z"${F}/>`,
	'skip-forward': `<path d="M16.5 5H19v14h-2.5z"${F}/><path d="M5 5v14l9.5-7z"${F}/>`,
	restart: '<path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/>',
	shuffle:
		'<path d="M3 6h2c5 0 9 12 14 12h2"/><path d="m18 15 3 3-3 3"/><path d="M3 18h2c5 0 9-12 14-12h2"/><path d="m18 3 3 3-3 3"/>',
	repeat:
		'<path d="m17 2 4 4-4 4"/><path d="M3 11V10a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/>',
	'repeat-1':
		'<path d="m17 2 4 4-4 4"/><path d="M3 11V10a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/><path d="M11 10h1v4"/>',
	'volume-2': `${SPK}<path d="M15.5 9.5a4 4 0 0 1 0 5"/><path d="M18 7a7.5 7.5 0 0 1 0 10"/>`,
	'volume-x': `${SPK}<path d="m16 9.5 5 5M21 9.5l-5 5"/>`
} satisfies Partial<Record<IconName, string>>;

export const FAVORITE_ICON_PATHS = {
	heart: HEART,
	'heart-filled': HEART.replace('<path', `<path${F}`)
} satisfies Partial<Record<IconName, string>>;

export const ICON_PATHS: Record<IconName, string> = {
	...TRANSPORT_ICON_PATHS,
	...FAVORITE_ICON_PATHS,
	plus: '<path d="M12 5v14M5 12h14"/>',
	check: '<path d="m4.5 12.5 5 5 10-11"/>',
	x: '<path d="M6 6l12 12M18 6 6 18"/>',
	'chevron-left': '<path d="m14.5 6-6 6 6 6"/>',
	'chevron-right': '<path d="m9.5 6 6 6-6 6"/>',
	'chevron-down': '<path d="m6 9.5 6 6 6-6"/>',
	'arrow-left': '<path d="M19 12H5"/><path d="m11 6-6 6 6 6"/>',
	'arrow-right': '<path d="M5 12h14"/><path d="m13 6 6 6-6 6"/>',
	search: '<circle cx="11" cy="11" r="7"/><path d="m20 20-3.8-3.8"/>',
	house: '<path d="m4 11 8-7 8 7"/><path d="M6 9.5V20h12V9.5"/>',
	library: '<path d="M4 3.5h16V21H4z"/><path d="M4 17.5h16"/><path d="M8.5 3.5V17.5M12.5 3.5V17.5"/><path d="M15.5 7v10.5"/>',
	'list-music':
		'<path d="M3.5 6h11M3.5 10h7M3.5 14h11M3.5 18h7"/><path d="M18 14v6"/><circle cx="16" cy="20" r="2"/>',
	'disc-3': '<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="2"/>',
	'mic-vocal':
		'<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5.5 11a6.5 6.5 0 0 0 13 0"/><path d="M12 17.5V21"/>',
	user: '<circle cx="12" cy="8" r="3.5"/><path d="M5 20a7 7 0 0 1 14 0"/>',
	settings:
		'<path d="M4 7h16M4 12h16M4 17h16"/><circle cx="9" cy="7" r="2.2"/><circle cx="15" cy="12" r="2.2"/><circle cx="8" cy="17" r="2.2"/>',
	bell: '<path d="M6 9a6 6 0 0 1 12 0c0 5 2 6 2 6H4s2-1 2-6"/><path d="M10 20a2 2 0 0 0 4 0"/>',
	'volume-1': `${SPK}<path d="M15.5 9.5a4 4 0 0 1 0 5"/>`,
	'list-video': `<path d="M9.5 6h11M9.5 10.5h11M9.5 15h11M9.5 19.5h11"/><path d="M3.5 4.8v4.4L7.5 7z"${F}/>`,
	lyrics: '<path d="M4 6h16M4 10h16M4 14h10M4 18h7"/>',
	maximize: '<path d="M8 3.5H3.5V8M16 3.5h4.5V8M8 20.5H3.5V16M16 20.5h4.5V16"/>',
	minimize: '<path d="M3.5 8H8V3.5M16 3.5V8h4.5M3.5 16H8v4.5M16 20.5V16h4.5"/>',
	'more-horizontal': `<path d="M5.5 12h.01M12 12h.01M18.5 12h.01" stroke-width="2.8"/>`,
	'more-vertical': `<path d="M12 5.5h.01M12 12h.01M12 18.5h.01" stroke-width="2.8"/>`,
	download: '<path d="M12 3.5V15"/><path d="m7 10.5 5 5 5-5"/><path d="M4.5 20.5h15"/>',
	upload: '<path d="M12 15V3.5"/><path d="m7 8 5-5 5 5"/><path d="M4.5 20.5h15"/>',
	clock: '<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5V12l3 2"/>',
	timer: '<circle cx="12" cy="13" r="7.5"/><path d="M12 9.5V13l2.5 1.5"/><path d="M9.5 2.5h5"/>',
	gauge: '<path d="M4 14a8 8 0 1 1 16 0"/><path d="m12 14 3.5-3.5"/><path d="M4 18h16"/>',
	pin: '<path d="M9 4h6l1 7 2.5 3v2h-13v-2L8 11z"/><path d="M12 16v5"/>',
	folder:
		'<path d="M3.5 6.5A1.5 1.5 0 0 1 5 5h5l2 2.5h7A1.5 1.5 0 0 1 20.5 9v9a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 18z"/>',
	'trash-2':
		'<path d="M4 7h16"/><path d="M9.5 7V4.5h5V7"/><path d="M6.5 7l1 13.5h9l1-13.5"/><path d="M10 11v6M14 11v6"/>',
	pencil: '<path d="m14.5 5.5 4 4L8 20l-5 1 1-5z"/><path d="m13 7 4 4"/>',
	'alert-triangle': '<path d="M12 3.5 22 20H2z"/><path d="M12 9.5V14"/><path d="M12 17.2v.3"/>',
	'wifi-off':
		'<path d="M5 10a11 11 0 0 1 4.5-2.4M12 5.5c2.4 0 4.7.8 6.5 2M8.5 13.5a6.5 6.5 0 0 1 5-1.6M12 18.5v.5"/><path d="m4 4 16 16"/>',
	'refresh-cw':
		'<path d="M20 12a8 8 0 0 1-14.2 5M4 12a8 8 0 0 1 14.2-5"/><path d="M18.5 3.5v4h-4M5.5 20.5v-4h4"/>',
	'grip-vertical': `<path d="M9 6h.01M15 6h.01M9 12h.01M15 12h.01M9 18h.01M15 18h.01" stroke-width="2.6"/>`,
	video: '<rect x="2.5" y="6.5" width="13" height="11" rx="2"/><path d="m15.5 10.5 6-3v9l-6-3"/>'
};
