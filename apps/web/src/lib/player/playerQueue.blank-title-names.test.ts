import { describe, expect, it } from 'vitest';
import { render } from 'svelte/server';
import PlayerQueue from './PlayerQueue.svelte';
import { createQueue, type Track } from './queue.js';

/**
 * Builder-owned pin for the queue-only filter's blank-title fallback: the
 * visible row renders a blank title as its stable track id, so the same
 * track's controls and now-playing line must name the id too — never a
 * blank accessible name. Companion to the protected acceptance bank in
 * `playerQueue.filter.acceptance.test.ts` (which must stay unmodified).
 *
 * Run from repository root:
 *   pnpm --dir apps/web test src/lib/player/playerQueue.blank-title-names.test.ts
 */

const TRACKS: Track[] = [
	{ id: 'queue-coral', title: 'Coral Night', artist: 'Mira', streamUrl: '' },
	{ id: 'unnamed-queue', title: '  ', artist: '', streamUrl: '' }
];

function body(): string {
	// Blank-title track is the current one, so now-playing must name it.
	return render(PlayerQueue, {
		props: { state: { ...createQueue(TRACKS), currentIndex: 1 } }
	}).body;
}

function hook(html: string, id: string): string {
	const match = new RegExp(
		`<([\\w-]+)\\b[^>]*data-testid="${id}"[^>]*>([\\s\\S]*?)<\\/\\1>`
	).exec(html);
	expect(match, `${id} presentation is missing`).not.toBeNull();
	return match?.[2].replace(/<[^>]*>/g, ' ').replace(/\s+/g, ' ').trim() ?? '';
}

describe('blank-title accessible names', () => {
	it('names the blank-title current track by id in now-playing', () => {
		expect(hook(body(), 'now-playing')).toContain('unnamed-queue');
	});

	it('names the blank-title row by id in every control label', () => {
		const html = body();
		for (const label of [
			'Move unnamed-queue up',
			'Move unnamed-queue down',
			'Play unnamed-queue next',
			'Remove unnamed-queue from queue'
		]) {
			expect(html, `aria-label "${label}" is missing`).toContain(`aria-label="${label}"`);
		}
	});
});
