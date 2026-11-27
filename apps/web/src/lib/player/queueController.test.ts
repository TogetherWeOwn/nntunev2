import { expect, it } from 'vitest';
import { createQueue, type Track } from './queue.js';
import { createQueueController } from './queueController.js';

it('revokes library source and pending queue work on a changed binding', () => {
	const track: Track = { id: 'recording', title: 'Track', artist: '', streamUrl: '/recording' };
	const queue = createQueueController(createQueue([track]));
	queue.setLibraryBinding('first');
	const { next, sources } = queue.prepareLibrary([track]);
	queue.publish(next, queue.begin(next, 'replace', true), sources);
	const source = queue.sourceForQueuedPlay(track.id);
	const empty = createQueue([]), pending = queue.begin(empty, 'clear'), revision = queue.revision;
	expect(queue.setLibraryBinding('first')).toBe(false);
	expect(source?.isCurrent()).toBe(true);
	expect(queue.setLibraryBinding('second')).toBe(true);
	expect(source?.isCurrent()).toBe(false);
	expect(queue.isCurrent(revision)).toBe(false);
	queue.publish(empty, pending, sources);
	expect(queue.snapshot).toBe(next);
	expect(queue.hasLibraryTrack(track.id)).toBe(false);
});
