const START_LEAD_SECONDS = 0.05;
const MAX_CROSSFADE_SECONDS = 5;

type SchedulerContext = Pick<BaseAudioContext, 'currentTime' | 'destination' | 'createBufferSource' | 'createGain'>;

interface ScheduledTrack {
	source: AudioBufferSourceNode;
	gain: GainNode;
	started: boolean;
}

/**
 * Standalone scheduler for one or two already-decoded audio buffers. The caller
 * owns the context and must have it ready to render. Each play replaces the
 * previous schedule, including sources whose start times are still in the future.
 */
export function createBufferScheduler(context: SchedulerContext) {
	const active = new Set<ScheduledTrack>();

	function release(track: ScheduledTrack): void {
		if (!active.delete(track)) return;
		track.source.onended = null;
		try { track.source.disconnect(); } catch { /* Still disconnect the gain. */ }
		try { track.gain.disconnect(); } catch { /* Keep releasing the other tracks. */ }
	}

	function prepare(buffer: AudioBuffer): ScheduledTrack {
		const gain = context.createGain();
		const source = context.createBufferSource();
		const track = { source, gain, started: false };
		active.add(track);
		source.onended = () => release(track);
		source.buffer = buffer;
		source.connect(gain);
		gain.connect(context.destination);
		return track;
	}

	function stop(): void {
		for (const track of active) {
			try { if (track.started) track.source.stop(); } catch { /* Disconnect even when stop fails. */ }
			release(track);
		}
	}

	return {
		/** Crossfade is opt-in, capped at five seconds and half of each buffer. */
		play(firstBuffer: AudioBuffer, nextBuffer?: AudioBuffer, crossfadeSeconds = 0): void {
			stop();
			try {
				const first = prepare(firstBuffer);
				const next = nextBuffer && prepare(nextBuffer);
				// Make both graph paths before taking one clock snapshot. Give the
				// audio thread time to receive both starts, even for short buffers.
				const start = context.currentTime + START_LEAD_SECONDS;
				const end = start + firstBuffer.duration;
				let nextStart = end;
				if (next && nextBuffer) {
					const overlap = Number.isFinite(crossfadeSeconds) && crossfadeSeconds > 0
						? Math.min(crossfadeSeconds, MAX_CROSSFADE_SECONDS, firstBuffer.duration / 2, nextBuffer.duration / 2)
						: 0;
					nextStart -= overlap;
					if (nextStart < end) {
						first.gain.gain.setValueAtTime(1, nextStart);
						first.gain.gain.linearRampToValueAtTime(0, end);
						next.gain.gain.setValueAtTime(0, nextStart);
						next.gain.gain.linearRampToValueAtTime(1, end);
					}
				}
				first.source.start(start);
				first.started = true;
				if (next) {
					next.source.start(nextStart);
					next.started = true;
				}
			} catch (error) {
				stop();
				throw error;
			}
		},
		stop
	};
}
