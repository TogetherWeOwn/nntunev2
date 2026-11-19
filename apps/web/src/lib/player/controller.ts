import { createMediaController, type NaturalEndSelection } from './mediaController.js';
import type { QueueState } from './queue.js';
import { createQueueController } from './queueController.js';

export function createPlayerController(
	initial: QueueState, naturalEnd: NaturalEndSelection, matchesNative?: (audio: HTMLAudioElement, source: string) => boolean
) {
	const queue = createQueueController(initial);
	return { queue, media: createMediaController(queue, naturalEnd, matchesNative) };
}
