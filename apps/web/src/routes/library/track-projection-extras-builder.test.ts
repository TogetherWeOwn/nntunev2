import { expect, test } from 'vitest';
import ts from 'typescript';
import { displayArtist, displayTitle } from '#lib/library/order.js';
import { appendSelection } from '#lib/library/selection.js';
import { resolveStreamUrl } from '#lib/library/stream-url.js';
import { createQueue, type Track } from '#lib/player/queue.js';

// Builder-owned unit regression: Library items may carry loudness or quality
// extras (gain, peak, format, bitrate, codec, sample rate, nested leveling or
// quality blobs) that no verified contract delivers. The production mappers
// must project only identity/title/fallback/artist/streamUrl and never pass
// unknown fields through, so nothing downstream can mistake them for applied
// ReplayGain or verified quality.
//
// Future schema warning: if a verified loudness or quality field is ever
// added to the Library contract, this file must change deliberately with that
// contract (explicit mapping plus a new expectation), not by loosening the
// exact-projection assertions below.
const sources = import.meta.glob('./+page.svelte', {
	query: '?raw', import: 'default', eager: true
}) as Record<string, string>;

type Mapper = (item: unknown) => Track;

function mappers(): { queuedTrackFor: Mapper; playableTrackFor: Mapper } {
	const source = sources['./+page.svelte'];
	const start = source.indexOf('>', source.indexOf('<script')) + 1;
	const parsed = ts.createSourceFile('library.ts', source.slice(start, source.indexOf('</script', start)), ts.ScriptTarget.Latest, true);
	const wanted = new Set(['queuedTrackFor', 'playableTrackFor']);
	const found = parsed.statements.filter((node) => ts.isFunctionDeclaration(node) && wanted.has(node.name?.text ?? ''));
	if (found.length !== wanted.size) throw new Error('Missing library track mapper');
	const compiled = ts.transpileModule(found.map((node) => node.getText(parsed)).join('\n'), {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
	}).outputText;
	return new Function(
		'displayTitle', 'displayArtist', 'resolveStreamUrl',
		`${compiled}\nreturn { queuedTrackFor, playableTrackFor };`
	)(displayTitle, displayArtist, resolveStreamUrl) as { queuedTrackFor: Mapper; playableTrackFor: Mapper };
}

const EXTRA_KEYS = [
	'trackGainDb', 'albumGainDb', 'trackPeak', 'albumPeak',
	'format', 'bitrate', 'codec', 'sampleRate', 'leveling', 'quality'
];
const ALLOWED = ['artist', 'id', 'streamUrl', 'title', 'titleIsFallback'];

const extrasVariants: Record<string, Record<string, unknown>> = {
	absent: {},
	null: Object.fromEntries(EXTRA_KEYS.map((key) => [key, null])),
	malformed: {
		trackGainDb: 'loud', albumGainDb: NaN, trackPeak: -Infinity, albumPeak: {},
		format: 42, bitrate: '320kbps', codec: ['flac'], sampleRate: -1,
		leveling: 'on', quality: []
	},
	plausible: {
		trackGainDb: -6.5, albumGainDb: -7.2, trackPeak: 0.98, albumPeak: 1.0,
		format: 'flac', bitrate: 1411, codec: 'flac', sampleRate: 44100,
		leveling: { mode: 'track', trackGainDb: -6.5, preventClipping: true },
		quality: { lossless: true, bitDepth: 16, codec: 'flac', bitrate: 1411 }
	},
	unknownNested: {
		replayGain: { track_gain: '-6.5 dB' }, loudness: { lufs: -14 },
		extra: { quality: { format: 'aac' } }
	}
};

function item(extras: Record<string, unknown>, base: Record<string, unknown> = {}) {
	return {
		id: 'row-1', title: 'Real Title', artist: 'Real Artist',
		streamUrl: '  https://cdn.example/audio/row-1.mp3  ', ...base, ...extras
	};
}

const EXPECTED_QUEUED: Track = { id: 'row-1', title: 'Real Title', artist: 'Real Artist', streamUrl: '' };
const EXPECTED_PLAYABLE: Track = { ...EXPECTED_QUEUED, streamUrl: 'https://cdn.example/audio/row-1.mp3' };

for (const [name, extras] of Object.entries(extrasVariants)) {
	test(`queuedTrackFor and playableTrackFor project exact fields with ${name} extras`, () => {
		const { queuedTrackFor, playableTrackFor } = mappers();
		const source = item(extras);
		const before = JSON.stringify(source, (_k, v) => (typeof v === 'number' && !Number.isFinite(v) ? String(v) : v));
		const queued = queuedTrackFor(source);
		const playable = playableTrackFor(source);
		expect(queued).toStrictEqual(EXPECTED_QUEUED);
		expect(playable).toStrictEqual(EXPECTED_PLAYABLE);
		for (const track of [queued, playable]) {
			expect(Object.keys(track).sort()).toEqual(['artist', 'id', 'streamUrl', 'title']);
			for (const key of [...EXTRA_KEYS, ...Object.keys(extras)]) expect(track).not.toHaveProperty(key);
		}
		// The input item is never mutated by mapping.
		expect(JSON.stringify(source, (_k, v) => (typeof v === 'number' && !Number.isFinite(v) ? String(v) : v))).toBe(before);
	});

	test(`fallback title and missing artist keep the exact projection with ${name} extras`, () => {
		const { queuedTrackFor, playableTrackFor } = mappers();
		const source = { id: 'row-2', title: '   ', ...extras };
		const queued = queuedTrackFor(source);
		expect(queued).toStrictEqual({
			id: 'row-2', title: displayTitle({ id: 'row-2', title: '   ' }),
			titleIsFallback: true, artist: '', streamUrl: ''
		});
		const playable = playableTrackFor(source);
		expect(playable).toStrictEqual({ ...queued, streamUrl: '' });
		for (const track of [queued, playable]) {
			expect(Object.keys(track).every((key) => ALLOWED.includes(key))).toBe(true);
		}
	});

	test(`appendSelection projects metadata-only tracks without ${name} extras`, () => {
		const loaded = [item(extras), item(extras, { id: 'row-3', title: '', artist: undefined })];
		const result = appendSelection(createQueue([]), ['row-1', 'row-3'], loaded);
		expect(result.added).toBe(2);
		expect(result.queue.tracks[0]).toStrictEqual(EXPECTED_QUEUED);
		const fallback = result.queue.tracks[1];
		expect(fallback).toStrictEqual({
			id: 'row-3', title: displayTitle({ id: 'row-3', title: '' }),
			titleIsFallback: true, artist: '', streamUrl: ''
		});
		for (const track of result.queue.tracks) {
			expect(Object.keys(track).every((key) => ALLOWED.includes(key))).toBe(true);
			for (const key of EXTRA_KEYS) expect(track).not.toHaveProperty(key);
		}
	});
}

test('the same extras on every mapper produce identical output to the extras-free item', () => {
	const { queuedTrackFor, playableTrackFor } = mappers();
	const clean = item({});
	for (const extras of Object.values(extrasVariants)) {
		expect(queuedTrackFor(item(extras))).toStrictEqual(queuedTrackFor(clean));
		expect(playableTrackFor(item(extras))).toStrictEqual(playableTrackFor(clean));
	}
});
