import { describe, expect, it } from 'vitest';
import { DEMO_TRACKS } from './queue.js';
import { MEDIA_BYTES, syntheticBody, trackSalt } from './demoMedia.js';

describe('demo media', () => {
	it('keeps the original bytes when no track is requested', () => {
		const body = syntheticBody();
		expect(body).toHaveLength(MEDIA_BYTES);
		expect(new TextDecoder().decode(body.slice(0, 4))).toBe('RIFF');
		expect(new TextDecoder().decode(body.slice(8, 12))).toBe('WAVE');
		expect(new TextDecoder().decode(body.slice(12, 16))).toBe('fmt ');
		expect(new TextDecoder().decode(body.slice(36, 40))).toBe('data');
	});

	it('serves distinct bytes for each queue track in the fixture', () => {
		const heads = DEMO_TRACKS.map((t) => {
			const id = new URL(t.streamUrl, 'http://x').searchParams.get('track');
			return Array.from(syntheticBody(id).slice(0, 64)).join(',');
		});
		expect(new Set(heads).size).toBe(DEMO_TRACKS.length);
	});

	it('maps unknown or inherited names to the default stream', () => {
		expect(trackSalt(null)).toBe(0);
		expect(trackSalt('nope')).toBe(0);
		expect(trackSalt('constructor')).toBe(0);
		expect(trackSalt('__proto__')).toBe(0);
	});
});
