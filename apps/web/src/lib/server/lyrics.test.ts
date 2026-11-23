import { describe, expect, it, vi } from 'vitest';
import { activeAttributions, plannedAttributions } from '../attributions/index.js';
import {
	bindVerifiedLocalLyrics, createLyricsResolver, cueAt, parseTimedLrc, type ExactLyricsProvider
} from './lyrics.js';

const track = {
	trackName: 'Synthetic Track', artistName: 'Fixture Artist', albumName: 'Unit Test',
	durationSeconds: 120
};
const lrc = '[00:01]Fixture cue';
const match = { kind: 'match', track, instrumental: false, syncedLyrics: lrc } as const;
const sidecar = { path: 'album/demo.lrc', lrc: '[00:02]Sidecar cue' };
const mediaPath = 'album/demo.flac';

describe('verified local lyrics binding', () => {
	const selection = Object.freeze({
		libraryScope: Symbol('fixture library'), trackId: 'fixture-recording',
		occurrenceKey: 'fixture-release:1', sourceRevision: 'fixture-bundle-v1'
	});
	// The producer reads the identity from the verified source independently of the selection.
	const identity = { ...selection };

	it('projects matching nonempty timed tags first, then a verified sidecar', () => {
		const verified = { identity, mediaPath, sidecar };
		expect(bindVerifiedLocalLyrics(selection, selection, { ...verified, embedded: lrc })).toEqual({
			trackId: selection.trackId,
			result: { source: 'embedded', cues: [{ atMs: 1000, text: 'Fixture cue' }] }
		});
		expect(bindVerifiedLocalLyrics(selection, selection, { ...verified, embedded: '[00:61]Invalid' })).toEqual({
			trackId: selection.trackId,
			result: { source: 'sidecar', cues: [{ atMs: 2000, text: 'Sidecar cue' }] }
		});
	});

	it('refuses missing, empty or unusable local evidence', () => {
		for (const evidence of [null, { identity, embedded: ' ' }, { identity, embedded: '[00:02]' },
			{ identity, embedded: '[00:61]Invalid' }, { identity, embedded: '[00:01][00:61]' },
			{ identity, mediaPath, sidecar: { ...sidecar, path: 'another-album/demo.lrc' } },
			{ identity, embedded: lrc, instrumental: true }]) {
			expect(bindVerifiedLocalLyrics(selection, selection, evidence)).toBeNull();
		}
	});

	it('requires the verified library, track, occurrence and source revision to match', () => {
		for (const mismatch of [{ libraryScope: Symbol('other library') }, { trackId: 'other-recording' },
			{ occurrenceKey: 'other-release:1' }, { sourceRevision: 'fixture-bundle-v0' }]) {
			expect(bindVerifiedLocalLyrics(selection, selection, {
				identity: { ...identity, ...mismatch }, mediaPath, sidecar
			})).toBeNull();
		}
		for (const invalid of [{ trackId: '' }, { occurrenceKey: ' ' }, { sourceRevision: 'x'.repeat(257) }]) {
			const badSelection = Object.freeze({ ...selection, ...invalid });
			expect(bindVerifiedLocalLyrics(badSelection, badSelection, { identity: badSelection, embedded: lrc })).toBeNull();
		}
	});

	it('discards a delayed result after clearing, advancing or reselecting the same track', () => {
		const delayed = { identity, embedded: lrc };
		const next = Object.freeze({ ...selection, trackId: 'next-recording', occurrenceKey: 'fixture-release:2' });
		const reselected = Object.freeze({ ...selection });
		expect(bindVerifiedLocalLyrics(selection, null, delayed)).toBeNull();
		expect(bindVerifiedLocalLyrics(selection, next, delayed)).toBeNull();
		expect(bindVerifiedLocalLyrics(selection, reselected, delayed)).toBeNull();
		expect(bindVerifiedLocalLyrics(reselected, reselected, { identity, embedded: lrc })?.trackId)
			.toBe(reselected.trackId);
	});
});

describe('timed LRC', () => {
	it('sorts stamps, handles fractions, offsets and clears, and finds the current cue', () => {
		const cues = parseTimedLrc([
			String.fromCharCode(0xfeff) + '[ti:Fixture]', '[00:03.001][00:01.25]Cue A', '[00:02.5]Cue B',
			'[00:02.500]Cue C', '[00:05]', '[offset:+250]'
		].join('\r\n'))!;
		expect(cues).toEqual([
			{ atMs: 1000, text: 'Cue A' }, { atMs: 2250, text: 'Cue B' },
			{ atMs: 2250, text: 'Cue C' }, { atMs: 2751, text: 'Cue A' },
			{ atMs: 4750, text: '' }
		]);
		expect(cueAt(cues, 999)).toBeNull();
		expect(cueAt(cues, 1000)?.text).toBe('Cue A');
		expect(cueAt(cues, 2750)?.text).toBe('Cue C');
		expect(cueAt(cues, 12_000)?.text).toBe('');
		expect(cueAt(cues, NaN)).toBeNull();
		expect(parseTimedLrc('[00:01]Later\n[00:00]Earlier\n[offset:1500]')).toEqual([
			{ atMs: 0, text: 'Earlier' }, { atMs: 0, text: 'Later' }
		]);
	});

	it('rejects absent, untimed, malformed and over-budget documents', () => {
		for (const text of [undefined, '', 'Plain text', '[00:02]  ', '[00:61]Bad stamp',
			'[00:01][00:61]', '[00:01]' + 'x'.repeat(65_536),
			'[00:01]'.repeat(1025) + 'Too many cues']) {
			expect(parseTimedLrc(text)).toBeNull();
		}
		expect(parseTimedLrc('[00:01]'.repeat(1024) + 'Within cap')).toHaveLength(1024);
	});
});

describe('lyrics resolution', () => {
	it('prefers valid embedded lyrics, then a same-directory, same-stem sidecar', async () => {
		const provider = vi.fn<ExactLyricsProvider>().mockResolvedValue(match);
		const resolve = createLyricsResolver(provider);
		expect(await resolve({ embedded: lrc, mediaPath, sidecar }, track)).toMatchObject({ source: 'embedded' });
		const bracketed = '[12:30 PM] Meet me';
		expect(await resolve({ embedded: `[00:01] ${bracketed}`, mediaPath, sidecar }, track)).toEqual({
			source: 'embedded', cues: [{ atMs: 1000, text: bracketed }]
		});
		for (const embedded of ['', 'Untimed', '[00:99]Bad stamp', '[00:01] [00:61]', '[00:01] [00:61']) {
			expect(await resolve({ embedded, mediaPath, sidecar }, track)).toMatchObject({ source: 'sidecar' });
		}
		expect(provider).not.toHaveBeenCalled();
		for (const candidate of [{ ...sidecar, path: 'other/demo.lrc' },
			{ ...sidecar, path: 'album/other.lrc' }, { ...sidecar, lrc: 'Untimed' }]) {
			expect(await resolve({ mediaPath, sidecar: candidate }, track)).toMatchObject({ source: 'lrclib' });
		}
		// A cached remote result cannot mask a newly available local source.
		expect(await resolve({ embedded: lrc }, track)).toMatchObject({ source: 'embedded' });
		expect(provider).toHaveBeenCalledTimes(1);
	});

	it('requires an opt-in provider and full metadata, and skips known instrumentals', async () => {
		expect(await createLyricsResolver()({}, track)).toBeNull();
		const provider = vi.fn<ExactLyricsProvider>().mockResolvedValue(match);
		const resolve = createLyricsResolver(provider);
		expect(await resolve({ instrumental: true, embedded: lrc }, track)).toBeNull();
		expect(await resolve({})).toBeNull();
		for (const invalid of [{ albumName: ' ' }, { trackName: 'x'.repeat(257) }, { durationSeconds: NaN }]) {
			expect(await resolve({}, { ...track, ...invalid })).toBeNull();
		}
		expect(provider).not.toHaveBeenCalled();
	});

	it('checks the exact release and duration and returns a planned source credit', async () => {
		const provider = vi.fn<ExactLyricsProvider>();
		for (const other of [{ trackName: 'Different Track' }, { artistName: 'Different Artist' },
			{ albumName: 'Another Edition' }, { durationSeconds: 122.01 }]) {
			provider.mockResolvedValueOnce({ ...match, track: { ...track, ...other } });
		}
		provider.mockResolvedValue({ ...match, track: { ...track, durationSeconds: 122 } });
		const resolve = createLyricsResolver(provider);
		for (let i = 0; i < 4; i++) expect(await resolve({}, track)).toBeNull();
		const withPrivateFields = { ...track, mediaPath, libraryId: 'fixture-library' };
		const result = await resolve({}, withPrivateFields);
		expect(provider).toHaveBeenLastCalledWith(track, expect.any(AbortSignal));
		expect(result?.source).toBe('lrclib');
		const id = result?.source === 'lrclib' ? result.attributionId : undefined;
		expect(plannedAttributions.find((entry) => entry.id === id)).toMatchObject({
			name: 'LRCLIB', sourceUrl: 'https://lrclib.net/', status: 'planned', license: 'not-assessed'
		});
		expect(activeAttributions.some((entry) => entry.id === id)).toBe(false);
	});

	it('caches positive results for one hour from completion, without sliding expiry', async () => {
		let time = 1_000;
		const provider = vi.fn<ExactLyricsProvider>().mockImplementationOnce(async () => {
			time += 2000;
			return match;
		}).mockResolvedValue({ kind: 'not-found' });
		const resolve = createLyricsResolver(provider, () => time);
		const first = await resolve({}, track);
		expect(first?.source).toBe('lrclib');
		time += 3_600_000 - 1;
		expect(await resolve({}, track)).toBe(first);
		expect(provider).toHaveBeenCalledTimes(1);
		time++;
		expect(await resolve({}, track)).toBeNull();
		expect(provider).toHaveBeenCalledTimes(2);
	});

	it.each([{ kind: 'not-found' } as const, { ...match, instrumental: true },
		{ ...match, syncedLyrics: null }])('caches confirmed absence for five minutes: %o', async (miss) => {
		let time = 0;
		const provider = vi.fn<ExactLyricsProvider>().mockResolvedValueOnce(miss).mockResolvedValue(match);
		const resolve = createLyricsResolver(provider, () => time);
		expect(await resolve({}, track)).toBeNull();
		time = 299_999;
		expect(await resolve({}, track)).toBeNull();
		expect(provider).toHaveBeenCalledTimes(1);
		time++;
		expect(await resolve({}, track)).toMatchObject({ source: 'lrclib' });
	});

	it('retries unavailable, rejected and malformed responses rather than caching misses', async () => {
		const provider = vi.fn<ExactLyricsProvider>()
			.mockResolvedValueOnce({ kind: 'unavailable' })
			.mockRejectedValueOnce(new Error('Synthetic timeout'))
			.mockResolvedValueOnce({ ...match, syncedLyrics: 'Untimed' })
			.mockResolvedValue(match);
		const resolve = createLyricsResolver(provider);
		for (let i = 0; i < 3; i++) expect(await resolve({}, track)).toBeNull();
		expect(await resolve({}, track)).toMatchObject({ source: 'lrclib' });
		expect(provider).toHaveBeenCalledTimes(4);
	});

	it('bounds a stalled lookup, signals cancellation and retries without caching a miss', async () => {
		vi.useFakeTimers();
		try {
			let finishLate!: (reply: typeof match) => void;
			const provider = vi.fn<ExactLyricsProvider>().mockReturnValueOnce(new Promise((done) => {
				finishLate = done;
			})).mockResolvedValue(match);
			const resolve = createLyricsResolver(provider);
			const pending = resolve({}, track);
			const signal = provider.mock.calls[0]?.[1];
			await vi.advanceTimersByTimeAsync(2000);
			expect(signal?.aborted).toBe(true);
			expect(await pending).toBeNull();
			finishLate(match);
			const hit = await resolve({}, track);
			expect(hit?.source).toBe('lrclib');
			expect(provider).toHaveBeenCalledTimes(2);
			expect(vi.getTimerCount()).toBe(0);
		} finally {
			vi.useRealTimers();
		}
	});

	it('evicts the oldest entry when the shared hit/miss capacity is exceeded', async () => {
		const provider = vi.fn<ExactLyricsProvider>().mockResolvedValueOnce(match)
			.mockResolvedValue({ kind: 'not-found' });
		const resolve = createLyricsResolver(provider, () => 0);
		expect(await resolve({}, track)).not.toBeNull();
		for (let i = 0; i < 128; i++) await resolve({}, { ...track, durationSeconds: i + 200 });
		expect(await resolve({}, { ...track, durationSeconds: 327 })).toBeNull();
		expect(provider).toHaveBeenCalledTimes(129);
		expect(await resolve({}, track)).toBeNull();
		expect(provider).toHaveBeenCalledTimes(130);
	});

	it('does not replace a concurrent positive result with a slower miss', async () => {
		let finishMiss!: (reply: { kind: 'not-found' }) => void;
		const provider = vi.fn<ExactLyricsProvider>().mockReturnValueOnce(new Promise((done) => {
			finishMiss = done;
		})).mockResolvedValue(match);
		const resolve = createLyricsResolver(provider, () => 0);
		const pending = resolve({}, track);
		const hit = await resolve({}, track);
		finishMiss({ kind: 'not-found' });
		expect(await pending).toBe(hit);
		expect(await resolve({}, track)).toBe(hit);
		expect(provider).toHaveBeenCalledTimes(2);
	});
});
